//! The repo knowledge graph (issue #1015): a colony the mothership launches on an opted-in
//! repository builds its Understand-Anything graph once (`/understand`, the pass the cost guard
//! refuses everywhere else) and again whenever main moves past it. Every colony on the repository
//! then finds the newest graph under `.ua/` — read-only when its base is the graph's commit, a
//! writable copy to refresh when its base is ahead (boot.rs) — and git-excluded either way. The
//! tick that watches main and the two read-only routes live here; the shape follows maps.rs.

use crate::{
    ApiResult, App, Shared, client_error,
    sessions::{self, Session, SessionStatus},
    util::{valid_repo, write_atomic},
};
use anyhow::{Context, Result, bail};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    path::{Path as FsPath, PathBuf},
    time::Duration,
};

/// The `origin` a graph colony carries, so its idle and its end are recognised.
pub const ORIGIN: &str = "ua-graph";
/// Where the graph colony writes the graph, as the colony sees it.
const OUT_FILE: &str = "knowledge-graph.json";
/// A graph the mothership stores; the graph itself can be large, but not unbounded.
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// How long a graph colony may sit idle without a valid graph before it gives its slot back,
/// like a mapping colony (maps.rs).
pub(crate) const GRAPH_IDLE_GRACE: Duration = Duration::from_secs(15 * 60);
/// How often the tick re-reads main of every opted-in repository.
const TICK: Duration = Duration::from_secs(10 * 60);
/// After a launch, how long the tick waits before launching the same repository again, however
/// the last colony ended: a repo whose pass keeps failing is retried at most once an hour.
const RETRY_AFTER: chrono::Duration = chrono::Duration::minutes(60);

fn valid_sha(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether the pinned plugin can run its pass: `/understand` executes
/// `packages/core/dist/index.js`, which the pin ships as TypeScript source only, so a host-side
/// prebuild (a follow-up issue) has to put it in the plugin root first.
fn ready(app: &App) -> bool {
    crate::understand_anything::dir(app)
        .join("packages/core/dist/index.js")
        .is_file()
}

/// The instructions a graph colony runs on: like a mapping colony it leaves the repository
/// untouched — the graph is copied to the out directory and the publish ends in `no_changes`.
fn graph_prompt(repo: &str) -> String {
    format!(
        "Build the Understand-Anything knowledge graph of {repo} as it is at HEAD, with the \
understand-anything plugin mounted at /opt/colonizer/plugins/{}.\n\n\
1. Run the `/understand` skill over /workspace. When /workspace/.ua/knowledge-graph.json is \
already there the pass is incremental: it resumes from .ua/meta.json's gitCommitHash instead of \
rebuilding from scratch.\n\
2. Do not wait for confirmations or questions: accept the default .understandignore and every \
other default the pass proposes, and make reasonable calls yourself.\n\
3. Copy /workspace/.ua/knowledge-graph.json to /harness/out/{OUT_FILE} and nowhere else.\n\
4. Change nothing else in the repository: no edits outside .ua/, no commits, and do not write \
/harness/out/pr.md. Keep any scratch files under /tmp.",
        crate::understand_anything::NAME
    )
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// `<data>/understand-anything/graphs/<owner>/<name>/`; `None` for anything not a plain
/// `owner/name`, so an owner or a commit can never climb out of it.
pub fn graph_dir(data_dir: &FsPath, repo: &str) -> Option<PathBuf> {
    let (owner, name) = valid_repo(repo).then(|| repo.split_once('/'))??;
    Some(data_dir.join("understand-anything").join("graphs").join(owner).join(name))
}

fn graph_path(data_dir: &FsPath, repo: &str, commit: &str) -> Option<PathBuf> {
    if !valid_sha(commit) {
        return None;
    }
    graph_dir(data_dir, repo).map(|dir| dir.join(format!("{commit}.json")))
}

/// The `latest.json` pointer beside a repository's graphs: `{commit, built_at}`.
fn latest(data_dir: &FsPath, repo: &str) -> Option<Value> {
    let bytes = std::fs::read(graph_dir(data_dir, repo)?.join("latest.json")).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value.is_object().then_some(value)
}

/// The commit of the newest stored graph for `repo`, when there is one.
fn latest_commit(data_dir: &FsPath, repo: &str) -> Option<String> {
    latest(data_dir, repo)?["commit"]
        .as_str()
        .filter(|c| valid_sha(c))
        .map(String::from)
}

/// Reads a graph colony's `knowledge-graph.json`, checks it and stores it versioned by its commit,
/// with a `latest.json` pointer beside it. The commit is the one the graph says it analysed
/// (`project.gitCommitHash`), else the colony's own HEAD — a graph colony commits nothing.
pub async fn ingest(app: &App, s: &Session) -> Result<String> {
    let file = app.session_dir(&s.id).join("out").join(OUT_FILE);
    let bytes = tokio::fs::read(&file)
        .await
        .with_context(|| format!("the graph colony wrote no {OUT_FILE}"))?;
    if bytes.len() > MAX_BYTES {
        bail!("{OUT_FILE} is larger than 32 MB");
    }
    let doc: Value = serde_json::from_slice(&bytes).with_context(|| format!("{OUT_FILE} is not JSON"))?;
    if !doc.get("nodes").is_some_and(Value::is_array) {
        bail!("{OUT_FILE} has no nodes array: not a knowledge graph");
    }
    let commit = match doc
        .pointer("/project/gitCommitHash")
        .and_then(Value::as_str)
        .filter(|c| valid_sha(c))
        .map(String::from)
    {
        Some(commit) => commit,
        None => head_of(app, s)
            .await
            .context("the graph names no commit and the colony's HEAD could not be read")?,
    };
    let dir = graph_dir(&app.cfg.data_dir, &s.repo).context("invalid repository name")?;
    let path = graph_path(&app.cfg.data_dir, &s.repo, &commit).context("the graph names no commit")?;
    tokio::fs::create_dir_all(&dir).await?;
    write_atomic(&path, &bytes).await?;
    // The pointer only ever advances: an older or resumed graph colony ending late — after a newer
    // graph was already stored — must not move `latest` back, so a pointer with a newer `built_at`
    // stands.
    let now = Utc::now();
    let newer_stored = latest(&app.cfg.data_dir, &s.repo)
        .and_then(|p| p["built_at"].as_str().map(String::from))
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(&at).ok())
        .is_some_and(|at| at.with_timezone(&Utc) > now);
    if !newer_stored {
        write_atomic(
            &dir.join("latest.json"),
            &serde_json::to_vec(&json!({"commit": commit, "built_at": now.to_rfc3339()}))?,
        )
        .await?;
    }
    Ok(commit)
}

/// The colony's HEAD, read host-side through the admin dir like a mapping colony's revision.
async fn head_of(app: &App, s: &Session) -> Option<String> {
    let admin = FsPath::new(s.git_admin_dir.as_deref()?);
    let mut cmd = app.git(admin);
    cmd.arg("--work-tree").arg(&s.worktree).args(["rev-parse", "HEAD"]);
    crate::util::exec_within(Duration::from_secs(5), &mut cmd)
        .await
        .ok()
        .map(|r| r.trim().to_string())
        .filter(|r| valid_sha(r))
}

/// Called when a graph colony's publish finishes, whatever the outcome.
pub async fn on_colony_end(app: &App, s: &Session) {
    if s.origin.as_deref() != Some(ORIGIN) {
        return;
    }
    match ingest(app, s).await {
        Ok(commit) => {
            app.session_log(&s.id, "info", format!("knowledge graph stored for {} at {commit}", s.repo))
                .await
        }
        Err(e) => app.session_log(&s.id, "error", format!("no knowledge graph: {e:#}")).await,
    }
}

/// A graph colony's turn ended: its product is the graph, so a valid one is stored and the colony
/// stops itself, like a mapping colony's. Without one it gets the grace window, then its slot back.
pub async fn on_idle(app: Shared, id: String) {
    on_idle_after(app, id, GRAPH_IDLE_GRACE).await;
}

pub(crate) async fn on_idle_after(app: Shared, id: String, grace: Duration) {
    let Some(s) = app.session(&id).await else { return };
    if s.origin.as_deref() != Some(ORIGIN) || s.status != SessionStatus::Idle {
        return;
    }
    if finish_if_built(&app, &s).await {
        return;
    }
    // Not built yet: the grace window, then the slot back. A session touched while idle restarts
    // the window rather than disarming the watchdog — an idle graph colony is still the repo's
    // only one, so one nothing ever stops would block the repository's tick forever.
    let mut idle_since = s.updated_at;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(grace).await;
            let Some(s) = app.session(&id).await else { return };
            if s.status != SessionStatus::Idle || s.origin.as_deref() != Some(ORIGIN) {
                return;
            }
            if s.updated_at != idle_since {
                idle_since = s.updated_at;
                continue;
            }
            if finish_if_built(&app, &s).await {
                return;
            }
            crate::lifecycle::stop_colony(
                &app,
                &s,
                |x| x.status == SessionStatus::Idle && x.updated_at == idle_since,
                "graph colony went idle without a valid knowledge graph".into(),
                format!(
                    "graph colony went idle without a valid knowledge-graph.json for {} minutes; stopping it to free its slot",
                    grace.as_secs() / 60
                ),
            )
            .await;
            return;
        }
    });
}

async fn finish_if_built(app: &Shared, s: &Session) -> bool {
    if ingest(app.as_ref(), s).await.is_err() {
        return false;
    }
    let idle_since = s.updated_at;
    crate::lifecycle::finish_colony(
        app,
        s,
        |x| x.status == SessionStatus::Idle && x.updated_at == idle_since,
        format!("knowledge graph stored for {}; stopping the graph colony", s.repo),
    )
    .await
}

// ---------------------------------------------------------------------------
// Launch and the tick
// ---------------------------------------------------------------------------

/// A terminal status for a graph colony. Unlike a mapping colony's, an `Idle` graph colony still
/// counts: one waiting out its grace window above is still the repo's graph colony, and the tick
/// must not launch a second beside it.
fn is_ended(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::NoChanges
            | SessionStatus::PrOpened
            | SessionStatus::Merged
            | SessionStatus::Closed
            | SessionStatus::Stopped
            | SessionStatus::Failed
    )
}

/// Whether `s` is this repository's live graph colony — launching, running, or idling in its
/// grace window.
fn is_graph_colony(s: &Session, repo: &str) -> bool {
    s.repo.eq_ignore_ascii_case(repo) && s.origin.as_deref() == Some(ORIGIN) && !is_ended(s.status)
}

/// The graph colonies of `repo` in the sessions list, ended ones included — the record the tick
/// and the status route read instead of a state file: the ids sum the graph colonies' cost from
/// the spend journal, the count is `jobs`, the newest `created_at` paces the tick's retries. A
/// colony an operator deletes leaves the sessions list and with it these counts; nothing else
/// prunes sessions, so the cost covers every retained colony.
fn graph_sessions<'a>(sessions: &'a [Session], repo: &str) -> Vec<&'a Session> {
    sessions
        .iter()
        .filter(|s| s.repo.eq_ignore_ascii_case(repo) && s.origin.as_deref() == Some(ORIGIN))
        .collect()
}

/// Launches the graph colony for `repo` through the ordinary admission path, refusing until the
/// plugin's dist is built. Returns the graph colony already running for the repository, if any.
async fn launch(app: &Shared, repo: &str) -> Result<Session, crate::AppError> {
    if !ready(app.as_ref()) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "the understand-anything plugin has no built packages/core/dist, so /understand cannot run yet (a host-side prebuild builds it)",
        ));
    }
    if let Some(s) = app.sessions.read().await.iter().find(|s| is_graph_colony(s, repo)).cloned() {
        return Ok(s);
    }
    let body = json!({
        "repo": repo,
        "title": format!("Knowledge graph: {repo}"),
        "instructions": graph_prompt(repo),
        "autopilot": true,
        "allow_duplicate": true,
        "origin": ORIGIN,
    });
    let new_session: sessions::NewSession =
        serde_json::from_value(body).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    let Json(session) = sessions::create(State(app.clone()), None, HeaderMap::new(), Json(new_session)).await?;
    Ok(session)
}

/// One tick at a time: the launches and the mirror fetches inside are slow, and a second would
/// only duplicate them.
static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let worker = app.clone();
            let done = tokio::spawn(async move { tick_once(&worker).await }).await;
            match done {
                Err(e) if e.is_panic() => {
                    tracing::warn!(error = %e, "ua-graph: the tick panicked and was skipped: {e}")
                }
                _ => {}
            }
        }
    });
}

/// Every org's graph repositories against main: where the newest stored graph is behind and no
/// colony is building one, launch one — the first run is a full analysis, later ones incremental.
async fn tick_once(shared: &Shared) {
    use std::sync::atomic::Ordering;
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
    let _guard = Guard;
    let app: &App = shared;
    if !ready(app) {
        tracing::info!("ua-graph: the understand-anything plugin has no built packages/core/dist; no graph colony can run yet");
        return;
    }
    for settings in shared.all_org_settings().into_values() {
        if settings.hidden || settings.ua_graph.is_empty() {
            continue;
        }
        // The org's list resolves like the docs loop's allowlist: repositories, whole orgs, `*`.
        for repo in crate::docs_loop::repos_of(shared, &settings.ua_graph).await.0 {
            let resolved = async {
                let bare = crate::code::ensure_bare(shared, &repo).await?;
                crate::code::resolve(shared, &bare, None).await.map(|(_, head)| head)
            }
            .await;
            let head = match resolved {
                Ok(head) => head,
                Err(e) => {
                    tracing::info!(repo = %repo, error = %format!("{e:#}"), "ua-graph: could not read main");
                    continue;
                }
            };
            // One reading of the sessions list covers both guards: a live colony for the repo,
            // and the retry pacing from the newest graph colony's launch.
            let live_or_recent = {
                let sessions = app.sessions.read().await;
                let jobs = graph_sessions(&sessions, &repo);
                jobs.iter().any(|s| !is_ended(s.status))
                    || jobs
                        .iter()
                        .map(|s| s.created_at)
                        .max()
                        .is_some_and(|at| Utc::now().signed_duration_since(at) < RETRY_AFTER)
            };
            if latest_commit(&app.cfg.data_dir, &repo).is_some_and(|c| c == head) || live_or_recent {
                continue;
            }
            if let Err(e) = launch(shared, &repo).await {
                tracing::info!(repo = %repo, error = e.message(), "ua-graph: no graph colony launched");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Boot: what a colony's worktree gets
// ---------------------------------------------------------------------------

/// What the boot does with the stored graph for one colony.
pub(crate) struct Placement {
    /// The commit the stored graph was built at.
    pub commit: String,
    /// A read-only bind when the colony's base is the graph's commit; a writable seed otherwise.
    pub read_only: bool,
    /// The stored graph file, host-side.
    pub source: PathBuf,
    /// The prompt note pointing the colony at the graph; `None` for the graph colony itself.
    pub note: Option<String>,
}

/// The mount-versus-seed decision: the graph colony always writes; a colony whose base is the
/// graph's commit reads it read-only; anything else — ahead of the graph, or a base that could not
/// be resolved — gets a writable copy, which is always safe. Pure, so it is tested.
pub(crate) fn mounts_read_only(graph_colony: bool, graph_commit: Option<&str>, base_sha: Option<&str>) -> bool {
    !graph_colony && graph_commit.is_some_and(|g| base_sha == Some(g))
}

/// The note that points a colony at the shared graph: a current one reads it read-only; a base
/// ahead of it refreshes only what changed. The graph colony gets none. Pure, so it is tested.
pub(crate) fn graph_note(graph_colony: bool, graph_commit: &str, base_sha: Option<&str>) -> Option<String> {
    if graph_colony {
        return None;
    }
    let short = &graph_commit[..graph_commit.len().min(7)];
    let mut text = format!(
        "\n<knowledge-graph>\nA shared Understand-Anything knowledge graph of this repository, built at {short}, is at .ua/knowledge-graph.json; answer /understand-chat and /understand-explain questions from it rather than re-analysing the repository.\n"
    );
    if let Some(base) = base_sha.filter(|b| *b != graph_commit) {
        let base_short = &base[..base.len().min(7)];
        text.push_str(&format!(
            "It is behind your base {base_short} — refresh only the changed files (git diff {graph_commit}..HEAD) with the file-scoped commands; do not run a full /understand.\n"
        ));
    }
    text.push_str("</knowledge-graph>\n");
    Some(text)
}

/// The stored graph a colony on an opted-in repository gets, if any.
pub(crate) async fn placement(app: &App, s: &Session, bare: &FsPath, base: &str, opted_in: bool) -> Option<Placement> {
    if !opted_in {
        return None;
    }
    let commit = latest_commit(&app.cfg.data_dir, &s.repo)?;
    let graph_colony = s.origin.as_deref() == Some(ORIGIN);
    let base_sha = crate::github::fork_sha(app, bare, base).await.ok();
    Some(Placement {
        read_only: mounts_read_only(graph_colony, Some(commit.as_str()), base_sha.as_deref()),
        note: graph_note(graph_colony, &commit, base_sha.as_deref()),
        source: graph_path(&app.cfg.data_dir, &s.repo, &commit)?,
        commit,
    })
}

/// Puts the stored graph where the colony's worktree needs it: a read-only bind over an empty
/// placeholder (the guest's mount needs a target, like the path policy's), or a writable copy
/// seeded beside a `meta.json` naming the commit, so the next `/understand` pass is incremental.
/// Seeding keeps what is already there: a resumed colony may have refreshed its own copy.
pub(crate) fn place(wt: &FsPath, p: &Placement, mounts: &mut Vec<crate::sandbox::Mount>) -> std::io::Result<()> {
    let ua = wt.join(".ua");
    std::fs::create_dir_all(&ua)?;
    let graph = ua.join("knowledge-graph.json");
    if p.read_only {
        if !graph.exists() {
            std::fs::write(&graph, b"")?;
        }
        mounts.push(crate::sandbox::Mount {
            source: p.source.clone(),
            target: "/workspace/.ua/knowledge-graph.json".into(),
            read_only: true,
        });
    } else if !graph.exists() {
        std::fs::copy(&p.source, &graph)?;
        std::fs::write(
            ua.join("meta.json"),
            serde_json::to_vec(&json!({"gitCommitHash": p.commit})).expect("a one-field object serialises"),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// `GET /api/understand-anything/graphs/{owner}/{name}`: whether the repo is opted in, the newest
/// stored graph, whether a graph colony is building one, and what the repo's graph colonies have
/// cost, summed from the spend journal.
pub async fn status(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let app = &*app;
    let repo = format!("{owner}/{name}");
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let pointer = latest(&app.cfg.data_dir, &repo);
    let sessions = app.sessions.read().await;
    let jobs = graph_sessions(&sessions, &repo);
    let ids: Vec<String> = jobs.iter().map(|s| s.id.clone()).collect();
    Ok(Json(json!({
        "enabled": crate::orgs::builds_ua_graph(&app.org_settings(&owner), &repo),
        "commit": pointer.as_ref().and_then(|p| p["commit"].as_str().filter(|c| valid_sha(c))),
        "built_at": pointer.as_ref().and_then(|p| p["built_at"].as_str()),
        "building": jobs.iter().any(|s| !is_ended(s.status)),
        "jobs": ids.len(),
        "cost_usd": crate::spend::sessions_cost_usd(&app.cfg.data_dir, &ids),
    })))
}

/// `GET /api/understand-anything/graphs/{owner}/{name}/knowledge-graph.json`: the newest stored
/// graph, served as data — never sniffed or run, like a stored chat image.
pub async fn graph(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> Result<Response, crate::AppError> {
    let app = &*app;
    let repo = format!("{owner}/{name}");
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let missing = || client_error(StatusCode::NOT_FOUND, "no knowledge graph is stored for this repository");
    let commit = latest_commit(&app.cfg.data_dir, &repo).ok_or_else(missing)?;
    let path = graph_path(&app.cfg.data_dir, &repo, &commit).ok_or_else(missing)?;
    let bytes = tokio::fs::read(&path).await.map_err(|_| missing())?;
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox")
        .body(Body::from(bytes))
        .expect("a fixed response"))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/understand-anything/graphs/{owner}/{name}", routing::get(status))
        .route(
            "/api/understand-anything/graphs/{owner}/{name}/knowledge-graph.json",
            routing::get(graph),
        )
}

/// This module's feature descriptor (`features.rs`).
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "ua_graph",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: Some(start_tasks),
};

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";

    fn graph_doc(commit: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"project": {"gitCommitHash": commit}, "nodes": [{"id": "src"}]})).unwrap()
    }

    async fn graph_colony(root: &FsPath, out: Option<&[u8]>) -> Shared {
        let app = crate::tests::test_app(root);
        let mut s = crate::sessions::tests::colony("acme", SessionStatus::Idle);
        s.id = "g1".into();
        s.origin = Some(ORIGIN.into());
        app.sessions.write().await.push(s);
        let dir = app.session_dir("g1").join("out");
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(bytes) = out {
            std::fs::write(dir.join(OUT_FILE), bytes).unwrap();
        }
        app
    }

    #[tokio::test]
    async fn an_idle_graph_colony_with_a_graph_stores_it_versioned_and_stops() {
        let root = std::env::temp_dir().join(format!("colonizer-ua-graph-ok-{}", crate::util::short_id()));
        let app = graph_colony(&root, Some(&graph_doc(COMMIT))).await;
        on_idle_after(app.clone(), "g1".into(), Duration::from_secs(60)).await;
        let s = app.session("g1").await.unwrap();
        assert_eq!(s.status, SessionStatus::Stopped, "done, so it gives its slot back");
        assert_eq!(s.error, None);
        let dir = graph_dir(&app.cfg.data_dir, "acme/repo").unwrap();
        assert!(
            dir.join(format!("{COMMIT}.json")).is_file(),
            "the graph is stored by its commit"
        );
        let pointer: Value = serde_json::from_slice(&std::fs::read(dir.join("latest.json")).unwrap()).unwrap();
        assert_eq!(pointer["commit"], COMMIT);
        assert!(pointer["built_at"].as_str().is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_graph_colony_without_a_valid_graph_waits_then_stops_with_an_error() {
        let root = std::env::temp_dir().join(format!("colonizer-ua-graph-bad-{}", crate::util::short_id()));
        let app = graph_colony(&root, Some(b"{ not json")).await;
        let grace = Duration::from_secs(60);
        on_idle_after(app.clone(), "g1".into(), grace).await;
        assert_eq!(
            app.session("g1").await.unwrap().status,
            SessionStatus::Idle,
            "it may still be writing the graph"
        );
        tokio::time::sleep(grace + Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let s = app.session("g1").await.unwrap();
        assert_eq!(s.status, SessionStatus::Stopped);
        assert_eq!(
            s.error.as_deref(),
            Some("graph colony went idle without a valid knowledge graph")
        );
        assert!(latest(&app.cfg.data_dir, "acme/repo").is_none(), "nothing invalid is stored");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(start_paused = true)]
    async fn a_graph_colony_touched_while_idle_is_still_stopped_after_its_grace() {
        let root = std::env::temp_dir().join(format!("colonizer-ua-graph-touched-{}", crate::util::short_id()));
        let app = graph_colony(&root, Some(b"{ not json")).await;
        let grace = Duration::from_secs(60);
        on_idle_after(app.clone(), "g1".into(), grace).await;
        tokio::time::sleep(grace / 2).await;
        // Any session write while idle moves `updated_at`; the watchdog's window restarts from it
        // instead of being lost — a colony nothing ever stops would block the repo forever.
        app.update_session("g1", |s| {
            s.instructions = "touched".into();
        })
        .await;
        tokio::time::sleep(2 * grace + Duration::from_secs(2)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            app.session("g1").await.unwrap().status,
            SessionStatus::Stopped,
            "the touch restarts the window, it does not disarm the watchdog"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_graph_without_a_nodes_array_or_a_commit_is_refused() {
        let root = std::env::temp_dir().join(format!("colonizer-ua-graph-shape-{}", crate::util::short_id()));
        let app = graph_colony(&root, Some(br#"{"project": {}}"#)).await;
        let s = app.session("g1").await.unwrap();
        assert!(ingest(&app, &s).await.is_err(), "no nodes array");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_late_older_colony_does_not_move_the_newest_graph_back() {
        let root = std::env::temp_dir().join(format!("colonizer-ua-graph-back-{}", crate::util::short_id()));
        let app = graph_colony(&root, Some(&graph_doc(COMMIT))).await;
        let dir = graph_dir(&app.cfg.data_dir, "acme/repo").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        // A pointer from a newer graph, timestamped ahead: this older colony ending last must
        // store its graph without moving `latest` back.
        let ahead = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        std::fs::write(
            dir.join("latest.json"),
            json!({"commit": OTHER, "built_at": ahead}).to_string(),
        )
        .unwrap();
        let s = app.session("g1").await.unwrap();
        assert_eq!(ingest(&app, &s).await.unwrap(), COMMIT);
        assert!(
            dir.join(format!("{COMMIT}.json")).is_file(),
            "the graph itself is still stored"
        );
        assert_eq!(
            latest_commit(&app.cfg.data_dir, "acme/repo").as_deref(),
            Some(OTHER),
            "the newer pointer stands"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn graphs_live_under_the_data_dir_per_repository_and_nowhere_else() {
        let data = FsPath::new("/data");
        assert_eq!(
            graph_dir(data, "acme/web").unwrap(),
            PathBuf::from("/data/understand-anything/graphs/acme/web")
        );
        assert!(graph_dir(data, "../x").is_none());
        assert!(graph_dir(data, "acme/../../x").is_none());
        assert!(graph_dir(data, "acme").is_none());
        assert!(
            graph_path(data, "acme/web", "../../etc/passwd").is_none(),
            "only a sha names a graph file"
        );
    }

    #[test]
    fn readiness_is_the_built_dist_file_in_the_plugin_root() {
        let root = std::env::temp_dir().join(format!("colonizer-ua-graph-ready-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let dist = crate::understand_anything::dir(&app).join("packages/core/dist");
        std::fs::create_dir_all(&dist).unwrap();
        assert!(!ready(&app), "the directory alone is not the built file");
        std::fs::write(dist.join("index.js"), "built").unwrap();
        assert!(ready(&app));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_graph_is_read_only_only_where_base_and_graph_agree() {
        let g = Some(COMMIT);
        assert!(mounts_read_only(false, g, Some(COMMIT)), "base == graph: read-only bind");
        assert!(
            !mounts_read_only(false, g, Some("fedcba9876543210")),
            "behind: a writable copy"
        );
        assert!(
            !mounts_read_only(false, g, None),
            "an unreadable base seeds: the copy is always safe"
        );
        assert!(!mounts_read_only(true, g, Some(COMMIT)), "the graph colony writes its own");
    }

    #[test]
    fn the_note_points_at_the_graph_and_warns_only_when_behind() {
        let current = graph_note(false, COMMIT, Some(COMMIT)).unwrap();
        assert!(current.contains("built at 0123456"));
        assert!(current.contains(".ua/knowledge-graph.json"));
        assert!(!current.contains("behind your base"));
        let behind = graph_note(false, COMMIT, Some(OTHER)).unwrap();
        assert!(behind.contains("behind your base fedcba9"));
        assert!(behind.contains(&format!("git diff {COMMIT}..HEAD")));
        assert!(
            graph_note(true, COMMIT, Some(COMMIT)).is_none(),
            "the graph colony gets no note"
        );
    }
}
