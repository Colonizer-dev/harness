//! The merge train (issue #671): an opt-in loop that squash-merges colonies' open pull requests —
//! one per repository per tick, oldest first, and only when everything around a pull request is
//! sound: the repository opted in, the base branch's own checks green, the pull request containing
//! the base branch's tip (GitHub reports CLEAN even without up-to-date branches), its own checks
//! green, and every commit authored by an allowed identity and saying nothing the operator set the
//! train to refuse. Behind and conflicted pull requests are the watcher's auto-rebase path's to fix
//! (`rebase.rs`); the train triggers that path itself only for a CLEAN pull request that is
//! nonetheless behind the base tip. The last tick's reading is served by `GET /api/merge-train`.

use crate::{
    App, Shared,
    activity::Entry,
    authority,
    github::{self, CiState, Mergeability, PrCommit, PrInfo, PrState},
    plugins::parse_list,
    publish,
    sessions::{Session, SessionStatus},
};
use axum::{Json, routing};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::LazyLock,
    time::Duration,
};
use tokio::sync::Mutex;

/// How far apart the train's ticks sit.
const TICK: Duration = Duration::from_secs(120);
/// The merge gets a deadline like every other `gh` call, so a hung `gh` cannot wedge the train.
const MERGE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long, and how far apart, the train re-reads the remaining pull requests' mergeability after
/// a merge, waiting for GitHub to finish recomputing it against the moved base.
const MERGEABILITY_POLLS: u32 = 10;
const MERGEABILITY_POLL_GAP: Duration = Duration::from_secs(3);

/// The publish module's merge-train settings, as read for one tick. Defaults everywhere: the train
/// is off unless the operator switched it on.
#[derive(Clone, Debug, Default, PartialEq)]
struct TrainSettings {
    default_on: bool,
    overrides: String,
    deny_orgs: String,
    authors: String,
    forbid: String,
}

async fn train_settings(app: &App) -> TrainSettings {
    let modules = app.modules.read().await.clone();
    let schema = crate::modules::schema_for("publish", &modules.publish.provider, &app.agents);
    let read = |key: &str| crate::config::setting_str(&modules.publish, &schema, key);
    TrainSettings {
        default_on: read("merge_train") == "on",
        overrides: read("merge_train_overrides"),
        deny_orgs: read("merge_train_deny_orgs"),
        authors: read("merge_train_authors"),
        forbid: read("merge_train_forbid"),
    }
}

/// Whether the train may merge in a repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TrainState {
    On,
    Off,
    Denied,
}

/// The denylist beats everything, a repository override beats an owner override, either beats the
/// install-wide switch, and a malformed entry reads as absent (validation refuses it at save time;
/// a hand-edited modules.json just drops it).
fn effective_state(settings: &TrainSettings, repo: &str) -> TrainState {
    let repo = repo.to_ascii_lowercase();
    let (owner, _) = repo.split_once('/').unwrap_or((repo.as_str(), ""));
    if parse_list(&settings.deny_orgs).iter().any(|o| o.eq_ignore_ascii_case(owner)) {
        return TrainState::Denied;
    }
    let mut chosen: Option<(u8, bool)> = None;
    for entry in parse_list(&settings.overrides) {
        let Some((key, value)) = entry.split_once('=') else { continue };
        let on = match value.trim().to_ascii_lowercase().as_str() {
            "on" => true,
            "off" => false,
            _ => continue,
        };
        let key = key.trim().to_ascii_lowercase();
        let rank = if key == repo {
            2
        } else if key == owner {
            1
        } else {
            continue;
        };
        if chosen.is_none_or(|(r, _)| rank > r) {
            chosen = Some((rank, on));
        }
    }
    if chosen.map(|(_, on)| on).unwrap_or(settings.default_on) {
        TrainState::On
    } else {
        TrainState::Off
    }
}

/// The `merge_train_overrides` grammar, checked at save time (modules.rs' `validate_settings`).
pub(crate) fn validate_overrides(raw: &str) -> Result<(), String> {
    for entry in parse_list(raw) {
        let Some((key, value)) = entry.split_once('=') else {
            return Err(format!("entry {entry:?} has no `=`; write owner=on|off or owner/repo=on|off"));
        };
        if !matches!(value.trim().to_ascii_lowercase().as_str(), "on" | "off") {
            return Err(format!("entry {entry:?} must end in =on or =off"));
        }
        let key = key.trim();
        if key.is_empty() || key.starts_with('/') || key.ends_with('/') || key.split('/').count() > 2 {
            return Err(format!("entry {entry:?} does not name an owner or owner/repo"));
        }
    }
    Ok(())
}

/// The base branch's own checks: a merge lands on the base, so a red base holds the whole train.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BaseCi {
    Green,
    Pending,
    Failing,
    #[default]
    Unknown,
}

/// Any failure fails, anything still running is pending, and a commit with no checks at all is
/// green — nothing is configured that could go red. Two missing readings are [`BaseCi::Unknown`].
fn base_ci_from(check_runs: Option<&Value>, status: Option<&Value>) -> BaseCi {
    if check_runs.is_none() && status.is_none() {
        return BaseCi::Unknown;
    }
    const FAILING: &[&str] = &[
        "FAILURE",
        "CANCELLED",
        "TIMED_OUT",
        "ACTION_REQUIRED",
        "STARTUP_FAILURE",
        "ERROR",
    ];
    let word = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().trim().to_ascii_uppercase();
    let mut pending = false;
    if let Some(runs) = check_runs.and_then(|v| v["check_runs"].as_array()) {
        for run in runs {
            let (state, conclusion) = (word(run, "status"), word(run, "conclusion"));
            if state.is_empty() && conclusion.is_empty() {
                continue;
            }
            if FAILING.contains(&conclusion.as_str()) {
                return BaseCi::Failing;
            }
            pending |= !state.is_empty() && state != "COMPLETED";
        }
    }
    if let Some(status) = status {
        if FAILING.contains(&word(status, "state").as_str()) {
            return BaseCi::Failing;
        }
        pending |= word(status, "state") == "PENDING";
    }
    if pending { BaseCi::Pending } else { BaseCi::Green }
}

/// `behind_by` from `GET /repos/{repo}/compare/{base}...{head}`: the commits the base tip has that
/// the pull request does not. `None` when the read says nothing usable — which fails closed.
fn behind_from_compare(compare: Option<&Value>) -> Option<u64> {
    compare?.get("behind_by")?.as_u64()
}

/// One pull request row of the train's reading.
#[derive(Clone, Debug, PartialEq, Serialize)]
struct PrRow {
    session: String,
    pr_url: String,
    title: String,
    status: PrStatus,
    reason: String,
}

/// What the train makes of one pull request, as the route names the states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PrStatus {
    /// Mergeable now, and first in this repository's train order.
    Next,
    WaitingCi,
    NeedsRebase,
    Waiting,
    Skipped,
    Merged,
}

/// One repository's reading, as the route reports it.
#[derive(Clone, Debug, Default, PartialEq)]
struct RepoRow {
    state: Option<TrainState>,
    base: Option<String>,
    base_ci: BaseCi,
    checked_at: Option<DateTime<Utc>>,
    last_merge: Option<(String, DateTime<Utc>)>,
    prs: Vec<PrRow>,
}

/// Everything the train remembers between ticks, in memory: a restart only costs the first tick's
/// history and the once-per-base-commit rebase guard.
#[derive(Default)]
struct Train {
    repos: BTreeMap<String, RepoRow>,
    /// Rebases already triggered, keyed `(session, base tip sha)`: the once-per-sha guard, so a
    /// base that has not moved is never re-triggered (the watcher's `rebase::rebase_due` property).
    rebased: HashSet<(String, String)>,
}

static TRAIN: LazyLock<Mutex<Train>> = LazyLock::new(|| Mutex::new(Train::default()));

/// Everything [`decide`] reads about one pull request.
#[derive(Clone, Debug, PartialEq)]
struct PrFacts {
    info: PrInfo,
    commits: Vec<PrCommit>,
    /// Commits the base tip has that the pull request does not, counted on GitHub; `None` when
    /// that could not be read, which fails closed — nothing merges unverified.
    behind_base: Option<u64>,
    /// Whether the pull request already targets the repository's default branch: a stacked child
    /// still based on its parent's open branch waits for the watcher's retarget.
    base_is_default: bool,
}

/// The author and attribution guards for one tick. An empty allowed list refuses everything; an
/// unreadable mothership identity holds the repository instead of building one.
#[derive(Clone, Debug, Default, PartialEq)]
struct Guards {
    allowed_authors: Vec<String>,
    forbidden: Vec<String>,
}

/// Why a pull request must be brought up to date before it can merge, and who does it: behind and
/// conflicted readings are the watcher's auto-rebase path's (it already acts on them); the train
/// triggers that same path itself only for a CLEAN pull request that is behind by commits.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NeedsRebase {
    detail: String,
    auto_rebase: bool,
}

/// What the train does with one candidate this tick.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Decision {
    Merge,
    WaitingCi,
    NeedsRebase(NeedsRebase),
    Waiting(String),
    Skipped(String),
}

impl Decision {
    /// The route's status for this decision.
    fn status(&self) -> PrStatus {
        match self {
            Decision::Merge => PrStatus::Next,
            Decision::WaitingCi => PrStatus::WaitingCi,
            Decision::NeedsRebase(_) => PrStatus::NeedsRebase,
            Decision::Waiting(_) => PrStatus::Waiting,
            Decision::Skipped(_) => PrStatus::Skipped,
        }
    }

    /// The reason the route shows for this decision.
    fn reason(&self) -> String {
        match self {
            Decision::Merge => "first in line; merges on a green tick".to_string(),
            Decision::WaitingCi => "the pull request's checks are still running".to_string(),
            Decision::NeedsRebase(nr) => nr.detail.clone(),
            Decision::Waiting(reason) | Decision::Skipped(reason) => reason.clone(),
        }
    }
}

/// The title prefixes and labels that hold a pull request out of the train, case-insensitively.
const HOLD_MARKERS: &[&str] = &["wip", "hold", "dnm", "do-not-merge", "do not merge"];

/// The WIP/HOLD mark on a pull request, from its title prefix or its labels; `None` when it carries
/// neither.
fn hold_marker(title: &str, labels: &[String]) -> Option<&'static str> {
    let title = title.trim().to_ascii_lowercase();
    for marker in HOLD_MARKERS {
        if title == *marker
            || title.starts_with(&format!("{marker}:"))
            || title.starts_with(&format!("{marker} "))
            || title.starts_with(&format!("[{marker}]"))
        {
            return Some(*marker);
        }
    }
    labels.iter().find_map(|label| {
        HOLD_MARKERS
            .contains(&label.trim().to_ascii_lowercase().as_str())
            .then_some("do-not-merge")
    })
}

/// The first commit author outside the allowed set, named for the log. A commit GitHub names no
/// author for is refused, and an empty allowed set refuses everything: the guard fails closed.
fn foreign_author(commits: &[PrCommit], allowed: &[String]) -> Option<String> {
    for commit in commits {
        if commit.authors.is_empty() {
            return Some("an unknown author".to_string());
        }
        for (login, email) in &commit.authors {
            // Both sides are lowercased where they are built; the case-insensitive match here is
            // so the guard does not depend on that having happened.
            if !allowed
                .iter()
                .any(|a| a.eq_ignore_ascii_case(login) || a.eq_ignore_ascii_case(email))
            {
                return Some(if login.is_empty() { email.clone() } else { login.clone() });
            }
        }
    }
    None
}

/// The first forbidden substring any commit message contains, matched case-insensitively.
fn forbidden_message<'a>(commits: &'a [PrCommit], forbidden: &'a [String]) -> Option<&'a str> {
    for commit in commits {
        let message = commit.message.to_ascii_lowercase();
        for f in forbidden {
            if message.contains(&f.to_ascii_lowercase()) {
                return Some(f);
            }
        }
    }
    None
}

/// Decides what the train does with one pull request this tick. Pure, so the whole table is
/// tested; the caller has already read everything this needs.
fn decide(facts: &PrFacts, guards: &Guards) -> Decision {
    let info = &facts.info;
    if info.state != PrState::Open {
        return Decision::Waiting("the pull request is no longer open".to_string());
    }
    if info.is_draft {
        return Decision::Skipped("the pull request is a draft".to_string());
    }
    if let Some(marker) = hold_marker(&info.title, &info.labels) {
        return Decision::Skipped(format!("the pull request is marked {marker}"));
    }
    // With no readable commits both guards below would pass vacuously; the train's guarantee is
    // that it looked, so it refuses.
    if facts.commits.is_empty() {
        return Decision::Skipped("no commits could be read".to_string());
    }
    if let Some(word) = forbidden_message(&facts.commits, &guards.forbidden) {
        return Decision::Skipped(format!(
            "a commit message contains {word:?}, which merge_train_forbid refuses"
        ));
    }
    if let Some(author) = foreign_author(&facts.commits, &guards.allowed_authors) {
        return Decision::Skipped(format!("a commit is authored by {author}, outside merge_train_authors"));
    }
    if !facts.base_is_default {
        let base = info.base_ref_name.as_deref().unwrap_or("another colony's branch");
        return Decision::Waiting(format!(
            "its base is {base}, not the repository's default branch: a stacked pull request merges once \
             the colony it is stacked on has merged and the watcher has retargeted this one"
        ));
    }
    match info.mergeability {
        Mergeability::Conflicted => Decision::NeedsRebase(NeedsRebase {
            detail: "the pull request conflicts with its base branch; the watcher's auto-rebase path handles it".to_string(),
            auto_rebase: false,
        }),
        Mergeability::Behind => Decision::NeedsRebase(NeedsRebase {
            detail: "the pull request is behind its base branch; the watcher's auto-rebase path brings it up to date".to_string(),
            auto_rebase: false,
        }),
        Mergeability::Unknown => Decision::Waiting("GitHub has not yet computed whether the pull request can merge".to_string()),
        Mergeability::Clean => {
            // CLEAN is not enough when the repository does not require up-to-date branches: the
            // branch must contain the base tip, and when that cannot be counted nothing merges.
            match facts.behind_base {
                Some(0) => match info.ci {
                    CiState::Pending => Decision::WaitingCi,
                    CiState::Failure => Decision::Skipped("the pull request's checks are failing".to_string()),
                    CiState::Success => Decision::Merge,
                    // The train's guarantee is green CI; a pull request nobody checks is not green.
                    CiState::NoChecks => Decision::Skipped("no checks ran on this pull request".to_string()),
                },
                Some(n) => Decision::NeedsRebase(NeedsRebase {
                    detail: format!(
                        "GitHub reports the branch clean, but it is {n} commit{} behind the base branch's tip; \
                         it merges only once it contains that tip",
                        if n == 1 { "" } else { "s" }
                    ),
                    auto_rebase: true,
                }),
                None => Decision::Waiting("could not verify the branch contains the base tip".to_string()),
            }
        }
    }
}

/// The `gh pr merge` invocation for one merge: squash, pinned to the exact head the train read, so
/// a push landing between the read and the merge cannot ride through, and `--delete-branch` only
/// when nothing is stacked on the branch.
fn merge_args(pr_url: &str, head_oid: &str, delete_branch: bool) -> Vec<String> {
    let mut args = vec![
        "pr".to_string(),
        "merge".to_string(),
        pr_url.to_string(),
        "--squash".to_string(),
        "--match-head-commit".to_string(),
        head_oid.to_string(),
    ];
    if delete_branch {
        args.push("--delete-branch".to_string());
    }
    args
}

/// Whether another open colony sits on this one's branch: a colony stacked on it (`parent`), or any
/// open pull request targeting the branch in the same repository. Its branch is then kept on merge,
/// so the child's pull request is not closed under it.
fn has_stacked_child(sessions: &[Session], s: &Session) -> bool {
    sessions.iter().any(|c| {
        c.id != s.id
            && c.repo == s.repo
            && c.status == SessionStatus::PrOpened
            && c.pr_url.is_some()
            && (c.parent.as_deref() == Some(s.id.as_str()) || c.base.as_deref() == Some(s.branch.as_str()))
    })
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    let app = app.clone();
    tokio::spawn(async move { run(app).await });
}

async fn run(app: Shared) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        // One panicked tick must not kill the loop silently: the spawn isolates it and the panic is said.
        let worker = app.clone();
        if let Err(e) = tokio::spawn(async move { tick_once(&worker).await }).await
            && e.is_panic()
        {
            eprintln!("merge train: the tick panicked and was skipped: {e}");
        }
    }
}

/// One tick: the open pull requests, grouped by repository, each repository read and, where
/// everything is green, one pull request merged. Repositories the train never touches are still
/// reported, so the route can say why nothing moves there.
async fn tick_once(app: &Shared) {
    // Bound to a local first: a read guard in the `for` expression would live for the whole tick and
    // deadlock against update_session's write lock.
    let sessions = app.sessions.read().await.clone();
    let mut by_repo: BTreeMap<&str, Vec<&Session>> = BTreeMap::new();
    for s in sessions
        .iter()
        .filter(|s| s.status == SessionStatus::PrOpened && s.pr_url.is_some())
    {
        by_repo.entry(s.repo.as_str()).or_default().push(s);
    }
    if by_repo.is_empty() {
        // Nothing to report and nothing to guard: the memory is dropped rather than serving
        // colonies that are gone.
        *TRAIN.lock().await = Train::default();
        return;
    }
    // Drop the once-per-sha guards of colonies that no longer exist.
    let live: HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    TRAIN.lock().await.rebased.retain(|(id, _)| live.contains(id.as_str()));
    let settings = train_settings(app).await;
    let any_on = by_repo.keys().any(|repo| effective_state(&settings, repo) == TrainState::On);
    // The default identity is read once per tick, and only when something could actually merge. An
    // unreadable identity holds every on-repository: it must not silently turn the author guard off.
    let own = if any_on {
        default_authors(app).await
    } else {
        Some(Vec::new())
    };
    let configured = parse_list(&settings.authors)
        .iter()
        .map(|a| a.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let guards = Guards {
        allowed_authors: if configured.is_empty() {
            own.unwrap_or_default()
        } else {
            configured
        },
        forbidden: parse_list(&settings.forbid),
    };
    for (repo, group) in by_repo {
        let state = effective_state(&settings, repo);
        if state != TrainState::On {
            let reason = if state == TrainState::Denied {
                "the merge train never merges in this org"
            } else {
                "the merge train is off for this repository"
            };
            hold(app, repo, &group, state, PrStatus::Skipped, reason, true).await;
        } else if guards.allowed_authors.is_empty() {
            hold(
                app,
                repo,
                &group,
                state,
                PrStatus::Waiting,
                "the mothership's identity could not be read",
                false,
            )
            .await;
        } else {
            train_repo(app, repo, &group, &guards).await;
        }
    }
}

/// The identities the train treats as its own when no authors are configured: the login the
/// mothership publishes as (`gh`'s viewer) and the noreply email every publish commit is written
/// with. `None` when that identity could not be read — the caller holds rather than merges.
async fn default_authors(app: &App) -> Option<Vec<String>> {
    let v = github::viewer(app).await.ok()?;
    let login = v["login"].as_str()?.to_ascii_lowercase();
    let mut out = vec![login.clone()];
    if let Some(id) = v["id"].as_i64() {
        out.push(format!("{id}+{login}@users.noreply.github.com"));
    }
    Some(out)
}

fn pr_row(s: &Session, title: String, status: PrStatus, reason: String) -> PrRow {
    let url = s.pr_url.clone().unwrap_or_default();
    PrRow {
        session: s.id.clone(),
        pr_url: url,
        title,
        status,
        reason,
    }
}

/// The same status and reason on every colony's row in one repository.
fn held_rows(group: &[&Session], status: PrStatus, reason: &str) -> Vec<PrRow> {
    let rows: Vec<PrRow> = group
        .iter()
        .map(|s| pr_row(s, s.issue_title.clone(), status, reason.into()))
        .collect();
    rows
}

/// An on-repository reading stamped now, for the paths that hold a repository without training it.
fn on_row(base: Option<String>, base_ci: BaseCi, prs: Vec<PrRow>) -> RepoRow {
    let mut row = RepoRow {
        base,
        base_ci,
        prs,
        ..RepoRow::default()
    };
    row.state = Some(TrainState::On);
    row.checked_at = Some(Utc::now());
    row
}

/// Stores a repository that is not being trained this tick — off, denied, or held by something the
/// train could not get past.
async fn hold(app: &Shared, repo: &str, group: &[&Session], state: TrainState, status: PrStatus, reason: &str, quiet: bool) {
    let mut row = on_row(None, BaseCi::Unknown, held_rows(group, status, reason));
    row.state = Some(state);
    save(app, repo, row, group, quiet).await;
}

/// Runs one repository's tick: the base branch's CI gate, the candidates in train order, at most
/// one merge, the bounded settle wait, and the reading stored for the route.
async fn train_repo(app: &Shared, repo: &str, group: &[&Session], guards: &Guards) {
    let now = Utc::now();
    let base = match github::default_branch(app, repo).await {
        Ok(base) => base,
        Err(e) => {
            let reason = format!("the repository's default branch could not be read ({e:#})");
            save(
                app,
                repo,
                on_row(None, BaseCi::Unknown, held_rows(group, PrStatus::Waiting, &reason)),
                group,
                false,
            )
            .await;
            return;
        }
    };
    // Gate the whole repository on its base branch's own checks before anything else: not green
    // means nothing merges here this tick, and no pull request is even read.
    let base_ci = base_ci_of(app, repo, &base).await;
    if base_ci != BaseCi::Green {
        let reason = match base_ci {
            BaseCi::Pending => "the base branch's own checks are still running",
            BaseCi::Failing => "the base branch's own checks are failing",
            _ => "the base branch's own checks could not be read",
        };
        save(
            app,
            repo,
            on_row(Some(base), base_ci, held_rows(group, PrStatus::Waiting, reason)),
            group,
            false,
        )
        .await;
        return;
    }
    // Train order: oldest pull request first. A colony whose PR opened before `pr_opened_at` was
    // recorded sorts first, which is the honest reading of "oldest we know of".
    let mut ordered: Vec<&Session> = group.to_vec();
    ordered.sort_by(|a, b| a.pr_opened_at.cmp(&b.pr_opened_at).then(a.id.cmp(&b.id)));
    let mut rows = Vec::with_capacity(ordered.len());
    let mut facts: Vec<Option<PrFacts>> = Vec::with_capacity(ordered.len());
    let mut first: Option<String> = None;
    for s in &ordered {
        let reading = read_pr(app, s, &base).await;
        // Issue #765: the train reads each head too; a moved one re-points the colony's links.
        if let Ok(f) = &reading {
            crate::commit_links::head_seen(app, &s.id, f.info.head_ref_oid.as_deref());
        }
        let (title, decision) = match &reading {
            Ok(f) => {
                let title = if f.info.title.trim().is_empty() {
                    s.issue_title.clone()
                } else {
                    f.info.title.clone()
                };
                // Only the head of the train is `next`: a later mergeable one waits its turn, since
                // the first merge moves the base out from under it.
                let mut decision = decide(f, guards);
                if decision == Decision::Merge {
                    if let Some(first) = first.clone() {
                        decision = Decision::Waiting(format!(
                            "mergeable, but {first} is first in this repository's train; this one follows once the base has moved"
                        ));
                    } else {
                        first = Some(s.id.clone());
                    }
                }
                (title, decision)
            }
            Err(reason) => (s.issue_title.clone(), Decision::Waiting(reason.clone())),
        };
        if let (Decision::NeedsRebase(nr), Ok(f)) = (&decision, &reading)
            && nr.auto_rebase
        {
            rebase_once(app, s, f).await;
        }
        facts.push(reading.ok());
        rows.push(pr_row(s, title, decision.status(), decision.reason()));
    }
    // At most one merge per repository per tick: the head of the train, if it read as mergeable.
    let mut merged = None;
    if let Some(i) = rows.iter().position(|row| row.status == PrStatus::Next) {
        let s = ordered[i];
        let mut say = |status: PrStatus, reason: String| {
            let title = std::mem::take(&mut rows[i].title);
            rows[i] = pr_row(s, title, status, reason);
        };
        let read = facts[i].as_ref();
        let head = read.and_then(|f| f.info.head_ref_oid.clone()).unwrap_or_default();
        if head.is_empty() {
            say(
                PrStatus::Waiting,
                "the head commit is unknown; merges are pinned to it".to_string(),
            );
        } else if authority::external_writes_blocked() {
            say(PrStatus::Waiting, publish::BLOCKED.to_string());
        } else {
            // Fresh sessions: another colony may have been stacked on this one since the tick began.
            let fresh = app.sessions.read().await.clone();
            let url = s.pr_url.clone().unwrap_or_default();
            let args = merge_args(&url, &head, !has_stacked_child(&fresh, s));
            match crate::util::exec_within(MERGE_TIMEOUT, &mut app.gh(args)).await {
                Ok(_) => {
                    say(PrStatus::Merged, "squash-merged by the merge train".to_string());
                    merged = Some((url, s.clone()));
                }
                Err(e) => say(PrStatus::Waiting, format!("the merge failed: {e:#}")),
            }
        }
    }
    if let Some((ref url, ref s)) = merged {
        app.session_log(&s.id, "info", format!("the merge train squash-merged {url}"))
            .await;
        let mut entry = Entry::new("publish.merge_train", "colony").colony(s);
        entry.detail = Some("squash-merged by the merge train".to_string());
        crate::activity::record(app, entry).await;
        // GitHub recomputes the remaining pull requests' mergeability after the base moved; wait
        // (bounded) so the next tick starts from real answers.
        let others: Vec<&Session> = ordered
            .iter()
            .zip(rows.iter())
            .filter(|(_, row)| row.status != PrStatus::Merged)
            .map(|(s, _)| *s)
            .collect();
        for _ in 0..MERGEABILITY_POLLS {
            let mut readings = Vec::with_capacity(others.len());
            for s in &others {
                readings.push(github::pr_info(app, s.pr_url.as_deref().unwrap_or_default()).await);
            }
            let settled = readings
                .iter()
                .all(|r| r.as_ref().is_ok_and(|info| info.mergeability != Mergeability::Unknown));
            if settled {
                break;
            }
            tokio::time::sleep(MERGEABILITY_POLL_GAP).await;
        }
    }
    let mut row = on_row(Some(base), base_ci, rows);
    row.last_merge = merged.as_ref().map(|(url, _)| (url.clone(), now));
    save(app, repo, row, group, false).await;
}

/// Everything [`decide`] needs about one candidate: the widened `pr_info`, the commits for the
/// guards, and the behind count — only worth its own request when GitHub itself reports nothing
/// wrong, since BEHIND and CONFLICTING decide the branch already.
async fn read_pr(app: &App, s: &Session, default_branch: &str) -> Result<PrFacts, String> {
    let url = s.pr_url.as_deref().unwrap_or_default();
    let info = github::pr_info(app, url)
        .await
        .map_err(|e| format!("checking the pull request failed ({e:#})"))?;
    let commits = github::pr_commits(app, url)
        .await
        .map_err(|e| format!("reading the pull request's commits failed ({e:#})"))?;
    let head = info.head_ref_name.as_deref().unwrap_or(&s.branch).to_string();
    let behind_base = if info.mergeability == Mergeability::Clean
        && let Some(base) = info.base_ref_name.as_deref()
    {
        // Colonies open same-repo pull requests, so head_ref_name resolves in the base repo; a compare error fails closed.
        github::gh_get_json(app, &format!("repos/{}/compare/{base}...{head}", s.repo))
            .await
            .ok()
            .and_then(|v| behind_from_compare(Some(&v)))
    } else {
        None
    };
    let base_on_pr = info.base_ref_name.as_deref();
    Ok(PrFacts {
        base_is_default: base_on_pr.is_some_and(|b| b.eq_ignore_ascii_case(default_branch)),
        behind_base,
        info,
        commits,
    })
}

/// The base branch's own checks: check runs plus the combined status of its tip, through the
/// conditional-request cache — `gh_get_json`, because a cache hit answers 304 with the cached body,
/// and filtering to 200 would read `unknown` forever after the first tick.
async fn base_ci_of(app: &App, repo: &str, base: &str) -> BaseCi {
    let read = |path: String| async move { github::gh_get_json(app, &path).await.ok() };
    let checks = read(format!("repos/{repo}/commits/{base}/check-runs")).await;
    let status = read(format!("repos/{repo}/commits/{base}/status")).await;
    base_ci_from(checks.as_ref(), status.as_ref())
}

/// Brings a clean-but-stale branch up to date through the watcher's auto-rebase path (`rebase.rs`),
/// at most once per (pull request, base tip): a live colony rebases itself inside its own microVM,
/// a stopped one gets the host's mechanical rebase — the same handling a pull request GitHub itself
/// reports behind or conflicted already gets from the watcher, so those the train only reports.
async fn rebase_once(app: &Shared, s: &Session, facts: &PrFacts) {
    // Issue #84: a rebase writes, so the kill-switch refuses before anything is armed or run.
    if authority::external_writes_blocked() {
        return;
    }
    let Some(sha) = facts.info.base_ref_oid.clone().filter(|sha| !sha.is_empty()) else {
        return;
    };
    let fresh = TRAIN.lock().await.rebased.insert((s.id.clone(), sha.clone()));
    if !fresh {
        return;
    }
    let lock = app.repo_lock(&s.repo).await;
    let _worktree = lock.lock().await;
    app.session_log(
        &s.id,
        "info",
        format!("the merge train is bringing the branch up to date with its base at {sha}"),
    )
    .await;
    match publish::attempt_auto_rebase(app, &s.id, Mergeability::Behind, None).await {
        publish::RebaseOutcome::Rebased(sha) | publish::RebaseOutcome::Woke(sha) => {
            app.session_log(&s.id, "info", format!("its branch is being rebased onto the base at {sha}"))
                .await;
        }
        publish::RebaseOutcome::Conflicted => {
            app.session_log(&s.id, "warn", "the rebase onto its base found conflicts".to_string())
                .await;
        }
        publish::RebaseOutcome::Failed => {
            app.session_log(
                &s.id,
                "warn",
                "the rebase onto its base failed; it stays needs_rebase".to_string(),
            )
            .await;
        }
        publish::RebaseOutcome::Gone | publish::RebaseOutcome::Skipped => {}
    }
}

/// Stores one repository's reading and says, in each colony's own log, every wait or skip whose
/// reading changed since the last tick (a first reading counts as changed, and a merge is said
/// where it happens, never here); skips reach the activity feed on the same edge. Off/denied
/// repositories are stored silently.
async fn save(app: &App, repo: &str, mut row: RepoRow, group: &[&Session], quiet: bool) {
    let previous = {
        let mut train = TRAIN.lock().await;
        let previous = train.repos.remove(repo);
        if let Some(prev) = &previous
            && row.last_merge.is_none()
        {
            row.last_merge = prev.last_merge.clone();
        }
        train.repos.insert(repo.to_string(), row.clone());
        previous
    };
    if quiet {
        return;
    }
    for pr in &row.prs {
        if pr.status == PrStatus::Merged {
            continue;
        }
        let unchanged = previous.as_ref().is_some_and(|prev| {
            prev.prs
                .iter()
                .any(|old| old.session == pr.session && old.status == pr.status && old.reason == pr.reason)
        });
        if unchanged {
            continue;
        }
        let level = if pr.status == PrStatus::Skipped { "warn" } else { "info" };
        let word = match pr.status {
            PrStatus::Next => "next",
            PrStatus::WaitingCi => "waiting for checks",
            PrStatus::NeedsRebase => "needs rebase",
            PrStatus::Waiting => "waiting",
            PrStatus::Skipped => "skipped",
            PrStatus::Merged => unreachable!("merged rows are said by the merge"),
        };
        app.session_log(&pr.session, level, format!("merge train, {word}: {}", pr.reason))
            .await;
        if pr.status == PrStatus::Skipped
            && let Some(s) = group.iter().find(|s| s.id == pr.session)
        {
            let mut entry = Entry::new("publish.merge_train", "colony").colony(s);
            entry.detail = Some(pr.reason.clone());
            crate::activity::record(app, entry).await;
        }
    }
}

/// `GET /api/merge-train`: the last tick's reading, per repository with open colony pull requests.
/// A memory read — no GitHub is asked, so the answer is exactly as fresh as the last tick.
async fn list() -> Json<Value> {
    let train = TRAIN.lock().await;
    let repos = train
        .repos
        .iter()
        .map(|(repo, row)| {
            json!({
                "repo": repo,
                "state": row.state.unwrap_or(TrainState::Off),
                "base": row.base.clone(),
                "base_ci": row.base_ci,
                "checked_at": row.checked_at.map(|t| t.to_rfc3339()),
                "last_merge": row.last_merge.as_ref().map(|(url, at)| json!({"pr_url": url, "at": at.to_rfc3339()})),
                "prs": row.prs.iter().map(|pr| json!({
                    "session": pr.session,
                    "pr_url": pr.pr_url,
                    "title": pr.title,
                    "status": pr.status,
                    "reason": pr.reason,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    Json(json!({ "repos": repos }))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    axum::Router::new().route("/api/merge-train", routing::get(list))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(overrides: &[(&str, &str)]) -> TrainSettings {
        let mut s = TrainSettings::default();
        for (key, value) in overrides {
            match *key {
                "merge_train" => s.default_on = *value == "on",
                "merge_train_overrides" => s.overrides = (*value).to_string(),
                "merge_train_deny_orgs" => s.deny_orgs = (*value).to_string(),
                "merge_train_authors" => s.authors = (*value).to_string(),
                "merge_train_forbid" => s.forbid = (*value).to_string(),
                _ => unreachable!(),
            }
        }
        s
    }

    fn commit(login: &str, message: &str) -> PrCommit {
        PrCommit {
            authors: vec![(login.to_string(), format!("331648616+{login}@users.noreply.github.com"))],
            message: message.to_string(),
        }
    }

    fn facts(mergeability: Mergeability, ci: CiState) -> PrFacts {
        PrFacts {
            info: PrInfo {
                state: PrState::Open,
                mergeability,
                merge_state_status: "CLEAN".to_string(),
                merged_at: None,
                base_ref_oid: None,
                created_at: None,
                ci,
                is_draft: false,
                title: "a change".to_string(),
                labels: Vec::new(),
                head_ref_name: Some("colonizer/issue-1".to_string()),
                head_ref_oid: Some("abc123".to_string()),
                base_ref_name: Some("main".to_string()),
            },
            commits: vec![commit("colonizer-settlers", "do the thing")],
            behind_base: Some(0),
            base_is_default: true,
        }
    }

    fn guards() -> Guards {
        Guards {
            allowed_authors: vec!["colonizer-settlers".to_string()],
            forbidden: vec!["co-authored-by: claude".to_string()],
        }
    }

    #[test]
    fn the_settings_default_off_rank_overrides_and_keep_the_denylist_absolute() {
        // The publish schema's train settings, all defaulting to the feature being off.
        let p = &crate::modules::schema_for("publish", "github-pr", &[])["properties"];
        assert_eq!(p["merge_train"]["default"], "off");
        assert_eq!(p["merge_train"]["enum"], json!(["off", "on"]));
        for key in [
            "merge_train_overrides",
            "merge_train_deny_orgs",
            "merge_train_authors",
            "merge_train_forbid",
        ] {
            assert_eq!(p[key]["default"], "", "{key} defaults to empty");
        }
        assert_eq!(p["merge_train_overrides"]["format"], "merge-train-overrides");

        // Off unless switched on; a repository override beats an owner override; the denylist beats
        // every override; a malformed entry reads as absent.
        assert_eq!(
            effective_state(&settings(&[]), "acme/widget"),
            TrainState::Off,
            "the train is opt-in"
        );
        let on = settings(&[("merge_train", "on")]);
        assert_eq!(effective_state(&on, "acme/widget"), TrainState::On);
        let owner_only = settings(&[("merge_train_overrides", "acme=on")]);
        assert_eq!(effective_state(&owner_only, "acme/widget"), TrainState::On);
        assert_eq!(effective_state(&owner_only, "other/widget"), TrainState::Off);
        let mixed = settings(&[("merge_train", "on"), ("merge_train_overrides", "acme=off, acme/widget=on")]);
        assert_eq!(effective_state(&mixed, "acme/widget"), TrainState::On);
        assert_eq!(effective_state(&mixed, "acme/other"), TrainState::Off);
        let case = settings(&[("merge_train_overrides", "ACME/Widget=ON")]);
        assert_eq!(effective_state(&case, "acme/widget"), TrainState::On);
        let denied = settings(&[
            ("merge_train", "on"),
            ("merge_train_overrides", "acme/widget=on"),
            ("merge_train_deny_orgs", "other, acme"),
        ]);
        assert_eq!(effective_state(&denied, "acme/widget"), TrainState::Denied);
        assert_eq!(effective_state(&denied, "elsewhere/widget"), TrainState::On);

        // The denylist matches case-insensitively, so it cannot be escaped by the entry's case.
        let case_denied = settings(&[
            ("merge_train", "on"),
            ("merge_train_overrides", "Acme=on"),
            ("merge_train_deny_orgs", "acme"),
        ]);
        assert_eq!(effective_state(&case_denied, "Acme/app"), TrainState::Denied);

        let garbage = settings(&[("merge_train_overrides", "acme=maybe, nonsense, acme/widget=on, =off")]);
        assert_eq!(
            effective_state(&garbage, "acme/widget"),
            TrainState::On,
            "the well-formed entry reads"
        );
        assert_eq!(
            effective_state(&garbage, "acme/other"),
            TrainState::Off,
            "garbage reads as absent"
        );
        assert!(validate_overrides("acme=on, acme/widget=off").is_ok());
        for bad in ["acme=maybe", "nonsense", "=off", "acme/x/y=on", "acme/=on", "/acme=on"] {
            assert!(validate_overrides(bad).is_err(), "{bad:?} should be refused");
        }
    }

    #[test]
    fn a_clean_green_pull_request_merges_and_everything_less_is_held_or_skipped() {
        let g = guards();
        assert_eq!(decide(&facts(Mergeability::Clean, CiState::Success), &g), Decision::Merge);
        // The train's guarantee is green CI: a pull request nobody checks is not green, even though
        // GitHub's CLEAN already says the repository's requirements are met.
        assert_eq!(
            decide(&facts(Mergeability::Clean, CiState::NoChecks), &g),
            Decision::Skipped("no checks ran on this pull request".to_string())
        );
        assert_eq!(decide(&facts(Mergeability::Clean, CiState::Pending), &g), Decision::WaitingCi);
        assert_eq!(
            decide(&facts(Mergeability::Clean, CiState::Failure), &g),
            Decision::Skipped("the pull request's checks are failing".to_string())
        );
        assert_eq!(
            decide(&facts(Mergeability::Unknown, CiState::Success), &g),
            Decision::Waiting("GitHub has not yet computed whether the pull request can merge".to_string())
        );
        // Fail closed: without a behind count nothing is merged, however clean GitHub says it is.
        let mut unverified = facts(Mergeability::Clean, CiState::Success);
        unverified.behind_base = None;
        assert_eq!(
            decide(&unverified, &g),
            Decision::Waiting("could not verify the branch contains the base tip".to_string())
        );
    }

    #[test]
    fn drafts_holds_and_unreadable_commits_are_skipped() {
        let g = guards();
        let mut draft = facts(Mergeability::Clean, CiState::Success);
        draft.info.is_draft = true;
        assert_eq!(
            decide(&draft, &g),
            Decision::Skipped("the pull request is a draft".to_string())
        );

        let mut held = facts(Mergeability::Clean, CiState::Success);
        held.info.title = "WIP: add the thing".to_string();
        assert_eq!(
            decide(&held, &g),
            Decision::Skipped("the pull request is marked wip".to_string())
        );
        held.info.title = "Add the thing".to_string();
        held.info.labels = vec!["do-not-merge".to_string()];
        assert_eq!(
            decide(&held, &g),
            Decision::Skipped("the pull request is marked do-not-merge".to_string())
        );
        // A word starting with a marker is not a mark.
        assert_eq!(hold_marker("wippy results", &[]), None);

        // With no readable commits both guards would pass vacuously; the train refuses instead.
        let mut unreadable = facts(Mergeability::Clean, CiState::Success);
        unreadable.commits = Vec::new();
        assert_eq!(
            decide(&unreadable, &g),
            Decision::Skipped("no commits could be read".to_string())
        );
    }

    #[test]
    fn behind_conflicted_and_clean_but_behind_all_need_a_rebase_never_a_merge() {
        let g = guards();
        let behind = decide(&facts(Mergeability::Behind, CiState::Success), &g);
        let conflicted = decide(&facts(Mergeability::Conflicted, CiState::Success), &g);
        // GitHub's own readings are the watcher's auto-rebase path's to fix; it acts on them
        // unconditionally for an open pull request, so the train only reports them.
        assert!(
            matches!(&behind, Decision::NeedsRebase(NeedsRebase { auto_rebase: false, .. }))
                && matches!(&conflicted, Decision::NeedsRebase(NeedsRebase { auto_rebase: false, .. })),
            "behind: {behind:?}, conflicted: {conflicted:?}"
        );
        // The maintainer's lesson: CLEAN is not enough when the repository does not require
        // up-to-date branches — this one the train brings up to date itself, once per base commit.
        let mut stale = facts(Mergeability::Clean, CiState::Success);
        stale.behind_base = Some(2);
        assert!(matches!(
            decide(&stale, &g),
            Decision::NeedsRebase(NeedsRebase { auto_rebase: true, .. })
        ));
        for decided in [behind, conflicted, decide(&stale, &g)] {
            assert_ne!(decided, Decision::Merge);
        }

        // A stacked child waits for its retarget rather than merging onto its parent's branch.
        let mut child = facts(Mergeability::Clean, CiState::Success);
        child.base_is_default = false;
        child.info.base_ref_name = Some("colonizer/issue-1-parent".to_string());
        assert!(matches!(decide(&child, &g), Decision::Waiting(_)));
    }

    #[test]
    fn the_identity_and_attribution_guards_refuse_outsiders_and_fail_closed() {
        let mut foreign = facts(Mergeability::Clean, CiState::Success);
        foreign.commits = vec![commit("someone-else", "do the thing")];
        assert!(matches!(decide(&foreign, &guards()), Decision::Skipped(_)));

        // Configured authors (a login here) are accepted, case-insensitively.
        let g = Guards {
            allowed_authors: vec!["Someone-Else".to_string()],
            forbidden: Vec::new(),
        };
        assert_eq!(decide(&foreign, &g), Decision::Merge);

        // An unknown author on an otherwise readable commit is refused, and an empty allowed list
        // refuses rather than disabling the guard.
        let mut unsigned = facts(Mergeability::Clean, CiState::Success);
        unsigned.commits = vec![PrCommit {
            authors: Vec::new(),
            message: "do the thing".to_string(),
        }];
        assert!(matches!(decide(&unsigned, &guards()), Decision::Skipped(_)));
        assert_eq!(foreign_author(&foreign.commits, &[]), Some("someone-else".to_string()));

        // The attribution guard matches case-insensitively.
        let mut attributed = facts(Mergeability::Clean, CiState::Success);
        attributed.commits = vec![commit(
            "colonizer-settlers",
            "Fix the leak\n\nCO-AUTHORED-BY: CLAUDE <noreply@anthropic.com>",
        )];
        assert!(matches!(decide(&attributed, &guards()), Decision::Skipped(_)));
    }

    #[test]
    fn a_stacked_child_keeps_its_branch_a_lone_one_loses_it_and_the_merge_pins_the_head() {
        let parent = Session {
            id: "parent".into(),
            branch: "colonizer/issue-9".into(),
            ..crate::sessions::tests::colony("acme", SessionStatus::PrOpened)
        };
        let mut by_parent = crate::sessions::tests::colony("acme", SessionStatus::PrOpened);
        by_parent.id = "child".into();
        by_parent.parent = Some("parent".into());
        by_parent.pr_url = Some("https://github.com/acme/widget/pull/10".into());
        assert!(has_stacked_child(&[parent.clone(), by_parent], &parent));

        let mut by_base = crate::sessions::tests::colony("acme", SessionStatus::PrOpened);
        by_base.id = "other".into();
        by_base.parent = None;
        by_base.base = Some("colonizer/issue-9".into());
        by_base.pr_url = Some("https://github.com/acme/widget/pull/11".into());
        assert!(has_stacked_child(&[parent.clone(), by_base.clone()], &parent));

        // A merged colony on the branch does not count.
        let mut gone = by_base;
        gone.status = SessionStatus::Merged;
        assert!(!has_stacked_child(&[parent.clone(), gone], &parent));

        // The merge is a squash pinned to the exact head that was read, deleting the branch only
        // when nothing is stacked on it.
        let url = "https://github.com/acme/widget/pull/9";
        assert_eq!(
            merge_args(url, "abc123", true),
            vec![
                "pr".to_string(),
                "merge".to_string(),
                url.to_string(),
                "--squash".to_string(),
                "--match-head-commit".to_string(),
                "abc123".to_string(),
                "--delete-branch".to_string(),
            ]
        );
        assert!(!merge_args(url, "abc123", false).contains(&"--delete-branch".to_string()));
    }

    #[test]
    fn the_base_ci_and_compare_parsers_fail_closed_on_missing_readings() {
        let runs = |conclusions: &[&str]| json!({"check_runs": conclusions.iter().map(|c| json!({"status": "COMPLETED", "conclusion": c})).collect::<Vec<_>>()});
        assert_eq!(base_ci_from(Some(&runs(&["success", "neutral"])), None), BaseCi::Green);
        assert_eq!(
            base_ci_from(Some(&runs(&["success"])), Some(&json!({"state": "success"}))),
            BaseCi::Green
        );
        assert_eq!(base_ci_from(Some(&runs(&["success", "failure"])), None), BaseCi::Failing);
        assert_eq!(
            base_ci_from(None, Some(&json!({"state": "error", "statuses": []}))),
            BaseCi::Failing,
            "the combined status fails on its own"
        );
        assert_eq!(
            base_ci_from(
                Some(&json!({"check_runs": [{"status": "IN_PROGRESS", "conclusion": ""}]})),
                None
            ),
            BaseCi::Pending
        );
        assert_eq!(base_ci_from(None, Some(&json!({"state": "pending"}))), BaseCi::Pending);
        // No checks configured on the base: vacuously green, so a repository without CI can merge.
        assert_eq!(
            base_ci_from(
                Some(&json!({"check_runs": []})),
                Some(&json!({"state": "expected", "statuses": []}))
            ),
            BaseCi::Green
        );
        // Both reads failed: unknown, and the repository's tick is held.
        assert_eq!(base_ci_from(None, None), BaseCi::Unknown);

        // The compare reader takes `behind_by` and nothing else; a missing one fails closed.
        assert_eq!(behind_from_compare(Some(&json!({"behind_by": 3, "ahead_by": 1}))), Some(3));
        assert_eq!(behind_from_compare(Some(&json!({"behind_by": 0}))), Some(0));
        assert_eq!(behind_from_compare(Some(&json!({}))), None);
        assert_eq!(behind_from_compare(None), None);
    }
}
