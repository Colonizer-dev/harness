//! Architecture maps: a repository drawn as an archify architecture diagram (github.com/tt-a1i/archify,
//! MIT, vendored as the `archify` skillset), which the cockpit renders as a nest — components as
//! chambers, boundaries as mounds, connections as tunnels — with each live colony's ants in the
//! chambers whose source files it is touching.
//!
//! A map is made by a colony, not by the mothership: `POST /api/maps/{owner}/{repo}` launches an
//! ordinary colony (origin `map`, autopilot on) whose instructions are to write a source-backed
//! architecture JSON to `/harness/out/architecture.json` and leave the repository untouched, so its
//! publish ends in `no_changes` and no pull request is opened. When it ends, the mothership reads the
//! file from the colony's out directory, keeps only the fields the cockpit draws after checking them,
//! and stores it at `<data>/maps/<owner>/<repo>.json`. `GET /api/maps/{owner}/{repo}` returns it,
//! picking up a finished mapping colony's file the first time it is asked if publish did not.
//!
//! `GET /api/touched` is where the ants go: for every live colony, the paths its worktree has changed
//! (uncommitted, plus committed against its base), read host-side the way the sibling brief does.

use crate::{
    ApiResult, App, Shared, client_error, github,
    sessions::{self, Session, SessionStatus},
    util::{exec_within, valid_repo, write_atomic},
};
use anyhow::{Context, Result, bail};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path as FsPath, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

/// The `origin` a mapping colony carries, so its end is recognised and the UI can label it.
pub const MAP_ORIGIN: &str = "map";
/// The skillset a mapping colony loads, whatever its org has switched on.
pub const ARCHIFY_SKILLSET: &str = "archify";

/// The agent settings a mapping colony runs with, over the install's own: one agent (no
/// delegation), medium effort, on Sonnet for every tier. Mapping is read-heavy and bounded, so the
/// orchestrator-plus-subagents shape only adds hand-off waits.
pub const MAP_AGENT_SETTINGS: &[(&str, &str)] = &[
    ("delegate", "off"),
    ("effort", "medium"),
    ("model", "claude-sonnet-5"),
    ("model_low", ""),
    ("model_high", ""),
];
/// Where the colony writes the map, as the colony sees it.
const OUT_FILE: &str = "architecture.json";

const MAX_BYTES: usize = 1024 * 1024;
const MAX_COMPONENTS: usize = 120;
const MAX_CONNECTIONS: usize = 400;
const MAX_BOUNDARIES: usize = 40;
const MAX_SOURCES: usize = 40;
const MAX_TEXT: usize = 160;
/// Files reported per colony: enough to place its ants, bounded so one colony rewriting a vendored
/// tree cannot bloat every poll.
const MAX_TOUCHED: usize = 200;
const TOUCHED_TTL: Duration = Duration::from_secs(4);
const PROBE_LIMIT: Duration = Duration::from_secs(2);

/// The instructions a mapping colony runs on. The repository must stay untouched: the publish that
/// follows then finds nothing to push and the colony ends in `no_changes`, with no pull request.
pub fn map_prompt(repo: &str) -> String {
    format!(
        "Map the architecture of {repo} as it is at HEAD, using the archify skill (loaded from \
/opt/colonizer/plugins/{ARCHIFY_SKILLSET}/skills/archify — read its SKILL.md first and follow its fast \
authoring path for the `architecture` type).\n\n\
Requirements:\n\
1. Read the repository before drawing: entry points, the main modules and services, storage, external \
systems and how they call each other. Every component except `external` ones must carry `sources` — \
repository-relative FILE paths (archify checks each is a file at the pinned revision; a directory \
fails), with a line where it helps: the files where that component really lives, its entry point \
first. 6 to 20 components, grouped with `boundaries` where the code has real seams. Set \
`meta.repository` to the repository URL and the revision `git -C /workspace rev-parse HEAD` prints.\n\
2. Write the diagram JSON to /harness/out/{OUT_FILE} and nowhere else. Validate it with \
`node /opt/colonizer/plugins/{ARCHIFY_SKILLSET}/skills/archify/bin/archify.mjs validate architecture \
/harness/out/{OUT_FILE} --repo-root /workspace --json` and fix what it reports until it passes.\n\
3. Do not change anything in /workspace: no edits, no new files, no commits, and do not write \
/harness/out/pr.md. Keep any scratch files under /tmp. This colony's output is the map alone; a \
change to the repository would open a pull request nobody asked for.\n\
4. Do not ask questions; make reasonable calls and note them in the diagram's `cards`."
    )
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn text(v: &Value, field: &str) -> Option<String> {
    let s = v.get(field)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.chars().take(MAX_TEXT).collect())
}

fn pair(v: &Value, field: &str) -> Option<[f64; 2]> {
    let a = v.get(field)?.as_array()?;
    match (a.first()?.as_f64(), a.get(1)?.as_f64(), a.len()) {
        (Some(x), Some(y), 2) if x.is_finite() && y.is_finite() && x.abs() < 1e5 && y.abs() < 1e5 => Some([x, y]),
        _ => None,
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

/// A repository-relative source path: no absolute path, no `..`, no backslashes, no control characters.
pub fn valid_source_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 400
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| part != "..")
}

/// Checks an archify architecture document and keeps only what the cockpit draws. Refuses anything
/// that is not an architecture diagram, has duplicate or malformed ids, dangling connections or
/// boundary members, unsafe source paths, or no source-backed component at all.
pub fn validate_map(doc: &Value) -> Result<Value> {
    if doc.get("diagram_type").and_then(Value::as_str) != Some("architecture") {
        bail!("not an archify architecture diagram (diagram_type must be \"architecture\")");
    }
    let components = doc
        .get("components")
        .and_then(Value::as_array)
        .context("the diagram has no components")?;
    if components.is_empty() || components.len() > MAX_COMPONENTS {
        bail!("the diagram must have between 1 and {MAX_COMPONENTS} components");
    }
    let mut ids = HashSet::new();
    let mut out_components = Vec::new();
    let mut sourced = 0;
    for c in components {
        let id = c.get("id").and_then(Value::as_str).unwrap_or_default();
        if !valid_id(id) {
            bail!("component id {id:?} is not a plain id");
        }
        if !ids.insert(id.to_string()) {
            bail!("component id {id:?} appears twice");
        }
        let pos = pair(c, "pos").with_context(|| format!("component {id:?} has no valid pos"))?;
        let size = pair(c, "size").unwrap_or([160.0, 60.0]);
        if size[0] <= 0.0 || size[1] <= 0.0 {
            bail!("component {id:?} has a non-positive size");
        }
        let mut sources = Vec::new();
        if let Some(list) = c.get("sources").and_then(Value::as_array) {
            for s in list.iter().take(MAX_SOURCES) {
                let path = s
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .trim_start_matches("./");
                if !valid_source_path(path) {
                    bail!("component {id:?} has an unsafe source path {path:?}");
                }
                let mut src = Map::new();
                src.insert("path".into(), json!(path));
                if let Some(line) = s.get("line").and_then(Value::as_u64).filter(|l| *l > 0) {
                    src.insert("line".into(), json!(line));
                }
                if let Some(label) = text(s, "label") {
                    src.insert("label".into(), json!(label));
                }
                sources.push(Value::Object(src));
            }
        }
        if !sources.is_empty() {
            sourced += 1;
        }
        let mut comp = Map::new();
        comp.insert("id".into(), json!(id));
        comp.insert("type".into(), json!(text(c, "type").unwrap_or_else(|| "backend".into())));
        comp.insert("label".into(), json!(text(c, "label").unwrap_or_else(|| id.to_string())));
        if let Some(sub) = text(c, "sublabel") {
            comp.insert("sublabel".into(), json!(sub));
        }
        comp.insert("pos".into(), json!(pos));
        comp.insert("size".into(), json!(size));
        comp.insert("sources".into(), Value::Array(sources));
        out_components.push(Value::Object(comp));
    }
    if sourced == 0 {
        bail!("no component carries sources: the map must be source-backed");
    }
    let mut out_connections = Vec::new();
    for c in doc
        .get("connections")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_CONNECTIONS)
    {
        let from = c.get("from").and_then(Value::as_str).unwrap_or_default();
        let to = c.get("to").and_then(Value::as_str).unwrap_or_default();
        if !ids.contains(from) || !ids.contains(to) {
            bail!("connection {from:?} → {to:?} names a component that does not exist");
        }
        let mut conn = Map::new();
        conn.insert("from".into(), json!(from));
        conn.insert("to".into(), json!(to));
        if let Some(label) = text(c, "label") {
            conn.insert("label".into(), json!(label));
        }
        out_connections.push(Value::Object(conn));
    }
    let mut out_boundaries = Vec::new();
    for b in doc
        .get("boundaries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_BOUNDARIES)
    {
        let wraps: Vec<&str> = b
            .get("wraps")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if wraps.is_empty() {
            continue;
        }
        if let Some(missing) = wraps.iter().find(|id| !ids.contains(**id)) {
            bail!("boundary wraps {missing:?}, which is not a component");
        }
        out_boundaries.push(json!({"label": text(b, "label").unwrap_or_default(), "wraps": wraps}));
    }
    let meta = doc.get("meta").cloned().unwrap_or(Value::Null);
    Ok(json!({
        "title": text(&meta, "title").unwrap_or_else(|| "Architecture".into()),
        "subtitle": text(&meta, "subtitle"),
        "components": out_components,
        "connections": out_connections,
        "boundaries": out_boundaries,
    }))
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// `<data>/maps/<owner>/<repo>.json`; `None` for anything that is not a plain `owner/repo`.
pub fn map_path(data_dir: &FsPath, repo: &str) -> Option<PathBuf> {
    if !valid_repo(repo) {
        return None;
    }
    let (owner, name) = repo.split_once('/')?;
    Some(data_dir.join("maps").join(owner).join(format!("{name}.json")))
}

fn read_stored(app: &App, repo: &str) -> Option<Value> {
    let path = map_path(&app.cfg.data_dir, repo)?;
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Reads a mapping colony's `architecture.json` from its out directory, checks it and stores it.
pub async fn ingest(app: &App, s: &Session) -> Result<Value> {
    let file = app.session_dir(&s.id).join("out").join(OUT_FILE);
    let bytes = tokio::fs::read(&file)
        .await
        .with_context(|| format!("the mapping colony wrote no {OUT_FILE}"))?;
    if bytes.len() > MAX_BYTES {
        bail!("{OUT_FILE} is larger than 1 MB");
    }
    let doc: Value = serde_json::from_slice(&bytes).with_context(|| format!("{OUT_FILE} is not JSON"))?;
    let map = validate_map(&doc)?;
    let revision = match s.git_admin_dir.as_deref() {
        Some(admin) => {
            let mut cmd = app.git(FsPath::new(admin));
            cmd.arg("--work-tree").arg(&s.worktree).args(["rev-parse", "HEAD"]);
            exec_within(PROBE_LIMIT, &mut cmd).await.ok().map(|r| r.trim().to_string())
        }
        None => None,
    }
    .or_else(|| {
        doc.pointer("/meta/repository/revision")
            .and_then(Value::as_str)
            .filter(|r| r.len() <= 64 && r.chars().all(|c| c.is_ascii_hexdigit()))
            .map(String::from)
    });
    let stored = json!({
        "repo": s.repo,
        "revision": revision,
        "generated_at": Utc::now().to_rfc3339(),
        "session": s.id,
        "map": map,
    });
    let path = map_path(&app.cfg.data_dir, &s.repo).context("invalid repository name")?;
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    write_atomic(&path, &serde_json::to_vec_pretty(&stored)?).await?;
    Ok(stored)
}

/// Called when a colony's publish finishes, whatever the outcome: a mapping colony's map is picked up.
pub async fn on_colony_end(app: &App, s: &Session) {
    if s.origin.as_deref() != Some(MAP_ORIGIN) {
        return;
    }
    match ingest(app, s).await {
        Ok(_) => {
            app.session_log(&s.id, "info", format!("architecture map stored for {}", s.repo))
                .await
        }
        Err(e) => app.session_log(&s.id, "error", format!("no architecture map: {e:#}")).await,
    }
}

fn is_ended(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::NoChanges
            | SessionStatus::PrOpened
            | SessionStatus::Merged
            | SessionStatus::Closed
            | SessionStatus::Stopped
            | SessionStatus::Failed
            | SessionStatus::Idle
    )
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn mapping_json(s: &Session) -> Value {
    json!({"id": s.id, "status": s.status, "created_at": s.created_at})
}

/// `GET /api/maps/{owner}/{repo}`: `{map, mapping}` — the stored map (or `null`) and the newest
/// mapping colony for the repository (or `null`), so the cockpit can show one in progress.
pub async fn get(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let repo = format!("{owner}/{name}");
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let sessions = app.sessions.read().await.clone();
    let newest = sessions
        .iter()
        .filter(|s| s.repo == repo && s.origin.as_deref() == Some(MAP_ORIGIN))
        .max_by_key(|s| s.created_at)
        .cloned();
    let mut stored = read_stored(&app, &repo);
    // A mapping colony that ended newer than the stored map — or with none stored — is picked up here,
    // for the colonies publish never reached (stopped, idle, or ended before a restart).
    if let Some(s) = newest.as_ref().filter(|s| is_ended(s.status)) {
        let newer = stored
            .as_ref()
            .and_then(|m| m.get("session").and_then(Value::as_str))
            .is_none_or(|id| id != s.id);
        if newer && let Ok(fresh) = ingest(&app, s).await {
            stored = Some(fresh);
        }
    }
    Ok(Json(json!({
        "repo": repo,
        "map": stored,
        "mapping": newest.as_ref().map(mapping_json),
    })))
}

/// Most paths the file tree sends; a larger repository is cut off with `truncated: true`.
const TREE_LIMIT: usize = 20_000;

/// `GET /api/maps/{owner}/{repo}/files`: every file path in the repository at the stored map's
/// revision (else the mothership's cached HEAD), read with `git ls-tree` from the local bare clone —
/// no GitHub call. The cockpit draws it as an explorer tree with the chosen component's files marked.
pub async fn files(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let repo = format!("{owner}/{name}");
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let revision = read_stored(&app, &repo)
        .and_then(|m| m.get("revision").and_then(Value::as_str).map(str::to_string))
        .filter(|r| r.chars().all(|c| c.is_ascii_hexdigit()) && !r.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());
    let bare = app.bare_repo(&repo);
    if !bare.is_dir() {
        return Err(client_error(
            StatusCode::NOT_FOUND,
            "this repository has no local clone yet; launch a colony on it first",
        ));
    }
    let mut cmd = app.git(&bare);
    cmd.args(["ls-tree", "-r", "--name-only", "-z", &revision]);
    let out = exec_within(Duration::from_secs(10), &mut cmd)
        .await
        .map_err(|e| client_error(StatusCode::NOT_FOUND, &format!("could not list {repo} at {revision}: {e:#}")))?;
    let mut paths: Vec<&str> = out.split('\0').filter(|p| !p.is_empty()).collect();
    let truncated = paths.len() > TREE_LIMIT;
    paths.truncate(TREE_LIMIT);
    Ok(Json(
        json!({"repo": repo, "revision": revision, "paths": paths, "truncated": truncated}),
    ))
}

/// `POST /api/maps/{owner}/{repo}`: launches a mapping colony through the ordinary admission path.
pub async fn create(State(app): State<Shared>, Path((owner, name)): Path<(String, String)>) -> ApiResult<Value> {
    let repo = format!("{owner}/{name}");
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    if crate::plugins::resolve(&app.cfg, ARCHIFY_SKILLSET).is_err() {
        return Err(client_error(
            StatusCode::CONFLICT,
            "the archify skillset is not installed with this app (scripts/fetch-vendor.sh stages it)",
        ));
    }
    let running = app
        .sessions
        .read()
        .await
        .iter()
        .find(|s| s.repo == repo && s.origin.as_deref() == Some(MAP_ORIGIN) && !is_ended(s.status))
        .cloned();
    if let Some(s) = running {
        return Ok(Json(json!({"repo": repo, "mapping": mapping_json(&s)})));
    }
    let body = json!({
        "repo": repo,
        "title": "Map the architecture",
        "instructions": map_prompt(&repo),
        "autopilot": true,
        "allow_duplicate": true,
        "origin": MAP_ORIGIN,
    });
    let new_session: sessions::NewSession =
        serde_json::from_value(body).map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")))?;
    let Json(session) = sessions::create(State(app.clone()), Json(new_session)).await?;
    Ok(Json(json!({"repo": repo, "mapping": mapping_json(&session)})))
}

/// Adds the archify skillset to a mapping colony's plugin list (the comma-separated `plugins`).
pub fn with_archify(plugins: &str) -> String {
    let mut names = crate::plugins::parse_list(plugins);
    if !names.iter().any(|n| n == ARCHIFY_SKILLSET) {
        names.push(ARCHIFY_SKILLSET.to_string());
    }
    names.join(",")
}

// ---------------------------------------------------------------------------
// Touched files
// ---------------------------------------------------------------------------

/// Every path from `git diff --name-only -z` output.
pub fn name_only_paths(out: &str) -> Vec<String> {
    out.split('\0')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect()
}

/// Merges the uncommitted and committed change sets, first seen first, capped.
pub fn merge_touched(uncommitted: Vec<String>, committed: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    uncommitted
        .into_iter()
        .chain(committed)
        .filter(|p| valid_source_path(p) && seen.insert(p.clone()))
        .take(MAX_TOUCHED)
        .collect()
}

async fn touched_for(app: &App, s: &Session) -> Option<Vec<String>> {
    let admin = FsPath::new(s.git_admin_dir.as_deref()?);
    let mut status = app.git(admin);
    status
        .arg("--work-tree")
        .arg(&s.worktree)
        .args(["status", "--porcelain", "-z", "--untracked-files=all"]);
    let uncommitted = exec_within(PROBE_LIMIT, &mut status)
        .await
        .map(|out| github::porcelain_paths(&out))
        .unwrap_or_default();
    let base = match s.base.as_deref().filter(|b| !b.is_empty()) {
        Some(b) => format!("origin/{b}"),
        None => "origin/HEAD".to_string(),
    };
    let mut diff = app.git(admin);
    diff.arg("--work-tree")
        .arg(&s.worktree)
        .args(["diff", "--name-only", "-z", &format!("{base}...HEAD")]);
    let committed = exec_within(PROBE_LIMIT, &mut diff)
        .await
        .map(|out| name_only_paths(&out))
        .unwrap_or_default();
    Some(merge_touched(uncommitted, committed))
}

static TOUCHED_CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);

/// How much of a colony's `events.jsonl` the reading probe looks at: the tail only.
const READ_TAIL_BYTES: u64 = 256 * 1024;
/// How many of a colony's most recent tool calls count as "reading now".
const READ_RECENT_CALLS: usize = 40;
/// Most paths reported per colony.
const READ_LIMIT: usize = 20;

/// The repository-relative paths a colony's most recent tool calls looked at, newest first and
/// deduplicated: `file_path`/`notebook_path`/`path` inputs (Read, Edit, Write, Glob, Grep, …) and
/// `/workspace/…` tokens in Bash commands. Paths outside the worktree are dropped. Pure over the
/// parsed event lines, so it is tested without a colony.
pub(crate) fn recent_reads(events: &[Value]) -> Vec<String> {
    let calls = events
        .iter()
        .rev()
        .filter(|e| e["type"] == "tool_call")
        .take(READ_RECENT_CALLS);
    let mut out: Vec<String> = Vec::new();
    for call in calls {
        for rel in call_paths(&call["input"]) {
            if !out.iter().any(|p| p == &rel) {
                out.push(rel);
            }
        }
    }
    out.truncate(READ_LIMIT);
    out
}

/// The repository-relative paths one tool call's input names, in order, deduplicated.
fn call_paths(input: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let Some(rel) = workspace_relative(raw) else { return };
        if !out.iter().any(|p| p == &rel) {
            out.push(rel);
        }
    };
    for key in ["file_path", "notebook_path", "path"] {
        if let Some(p) = input[key].as_str() {
            push(p);
        }
    }
    if let Some(cmd) = input["command"].as_str() {
        let in_worktree = cmd.contains("/workspace");
        for token in
            cmd.split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '"' | '\'' | '(' | ')' | '>' | '<' | '='))
        {
            let token = token.trim_end_matches([':', ',']);
            if token.starts_with("/workspace/") {
                push(token);
            } else if in_worktree && looks_like_repo_path(token) {
                // `cd /workspace && grep -n x crates/colonizer/src/…`: relative paths count too.
                push(token);
            }
        }
    }
    out
}

/// Most activity entries a file's detail lists per colony.
const FILE_ACTIVITY_LIMIT: usize = 15;
/// Largest diff a file's detail sends per colony; past it the diff is cut and flagged.
const FILE_DIFF_LIMIT: usize = 200 * 1024;
/// How long one colony's diff may take before the detail answers without it.
const FILE_DIFF_TIME: Duration = Duration::from_secs(5);

/// A `?path=` for the file detail: repository-relative, no `..`, no leading `/`, at most 1024
/// characters. Returns the normalised path (a leading `./` dropped), or `None` when it is refused.
pub(crate) fn valid_file_query(raw: &str) -> Option<String> {
    let path = raw.trim();
    let path = path.strip_prefix("./").unwrap_or(path).trim_end_matches('/');
    let ok = !path.is_empty()
        && path.len() <= 1024
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !part.is_empty() && part != ".." && part != ".");
    ok.then(|| path.to_string())
}

/// The last path segment, for short summaries.
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Counts lines the way a diff does: an empty string is none, a trailing newline adds none.
fn line_count(text: &str) -> usize {
    if text.is_empty() { 0 } else { text.lines().count().max(1) }
}

/// One line saying what a tool call did to `path`: "Read gateway.rs:120-180", "Edit (−3 +7)",
/// "Grep \"reserve\" in gateway.rs", "Bash: cargo test gateway".
fn call_summary(tool: &str, input: &Value, path: &str) -> String {
    let file = base_name(path);
    let short = |s: &str, n: usize| {
        let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if s.chars().count() > n {
            format!("{}…", s.chars().take(n).collect::<String>())
        } else {
            s
        }
    };
    match tool {
        "Read" => match (input["offset"].as_u64(), input["limit"].as_u64()) {
            (Some(o), Some(l)) => format!("Read {file}:{o}-{}", o + l),
            (Some(o), None) => format!("Read {file}:{o}-"),
            _ => format!("Read {file}"),
        },
        "Edit" => format!(
            "Edit (\u{2212}{} +{})",
            line_count(input["old_string"].as_str().unwrap_or_default()),
            line_count(input["new_string"].as_str().unwrap_or_default())
        ),
        "MultiEdit" => format!("Edit ×{}", input["edits"].as_array().map_or(0, Vec::len)),
        "Write" => format!(
            "Write {file} ({} lines)",
            line_count(input["content"].as_str().unwrap_or_default())
        ),
        "NotebookEdit" => format!("Edit notebook {file}"),
        "Grep" => format!(
            "Grep \"{}\" in {file}",
            short(input["pattern"].as_str().unwrap_or_default(), 40)
        ),
        "Glob" => format!("Glob {}", short(input["pattern"].as_str().unwrap_or(file), 50)),
        "Bash" => format!(
            "Bash: {}",
            short(
                input["command"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches("cd /workspace && "),
                70
            )
        ),
        other => format!("{other} {file}"),
    }
}

/// A colony's most recent tool calls that name `path`, newest first: `{ts, tool, summary, agent}`,
/// where `agent` is the subagent (settler) the call ran in, when the event says. Pure over the
/// parsed event lines, so it is tested without a colony.
pub(crate) fn file_activity(events: &[Value], path: &str) -> Vec<Value> {
    events
        .iter()
        .rev()
        .filter(|e| e["type"] == "tool_call")
        .filter(|e| call_paths(&e["input"]).iter().any(|p| p == path))
        .take(FILE_ACTIVITY_LIMIT)
        .map(|e| {
            let tool = e["name"].as_str().unwrap_or("tool");
            let agent = e["agent"]["description"].as_str().or_else(|| e["agent"]["name"].as_str());
            json!({"ts": e["ts"], "tool": tool, "summary": call_summary(tool, &e["input"], path), "agent": agent})
        })
        .collect()
}

/// A diff cut to at most `limit` bytes, on a line boundary; the flag says whether it was cut.
fn cap_diff(diff: String, limit: usize) -> (String, bool) {
    if diff.len() <= limit {
        return (diff, false);
    }
    let mut cut = limit;
    while !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    let end = diff[..cut].rfind('\n').map_or(cut, |i| i + 1);
    (diff[..end].to_string(), true)
}

/// A unified "new file" diff for an untracked file's text, as `git diff` would print it.
fn new_file_diff(path: &str, text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = format!(
        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for line in lines {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// What a colony changed in `path` since it branched: committed and uncommitted edits together
/// (`git diff <merge-base>` against the work tree), or the whole file for a new untracked one.
/// `None` when it has not changed the file, or the probe fails or times out.
async fn file_diff(app: &App, s: &Session, path: &str) -> Option<String> {
    let admin = FsPath::new(s.git_admin_dir.as_deref()?);
    let base = match s.base.as_deref().filter(|b| !b.is_empty()) {
        Some(b) => format!("origin/{b}"),
        None => "origin/HEAD".to_string(),
    };
    let mut merge_base = app.git(admin);
    merge_base
        .arg("--work-tree")
        .arg(&s.worktree)
        .args(["merge-base", &base, "HEAD"]);
    let from = exec_within(FILE_DIFF_TIME, &mut merge_base).await.ok()?.trim().to_string();
    if from.is_empty() || !from.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut diff = app.git(admin);
    diff.arg("--work-tree")
        .arg(&s.worktree)
        .args(["diff", "--no-color", "--no-ext-diff", &from, "--", path]);
    let out = exec_within(FILE_DIFF_TIME, &mut diff).await.ok()?;
    if !out.trim().is_empty() {
        return Some(out);
    }
    // An untracked file has no diff; show it as new. Only a regular file: the work tree is
    // colony-written, so a symlink there must not read a host file into the cockpit.
    let mut status = app.git(admin);
    status
        .arg("--work-tree")
        .arg(&s.worktree)
        .args(["status", "--porcelain", "--untracked-files=all", "--", path]);
    let porcelain = exec_within(FILE_DIFF_TIME, &mut status).await.ok()?;
    if !porcelain.starts_with("??") {
        return None;
    }
    let full = FsPath::new(&s.worktree).join(path);
    let meta = tokio::fs::symlink_metadata(&full).await.ok()?;
    if !meta.is_file() || meta.len() as usize > FILE_DIFF_LIMIT {
        return None;
    }
    let text = tokio::fs::read_to_string(&full).await.ok()?;
    Some(new_file_diff(path, &text))
}

#[derive(serde::Deserialize)]
pub struct FileQuery {
    path: String,
}

/// `GET /api/maps/{owner}/{repo}/file?path=…`: every live colony on the repository that is changing
/// or reading `path` — its recent tool calls on the file and its diff of it — for the map's
/// explorer pane. Live, so not cached.
pub async fn file(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<FileQuery>,
) -> ApiResult<Value> {
    let repo = format!("{owner}/{name}");
    if !valid_repo(&repo) {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let Some(path) = valid_file_query(&query.path) else {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "path must be repository-relative, without '..'",
        ));
    };
    let live: Vec<Session> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.repo == repo && (s.status.is_live() || s.status == SessionStatus::Publishing))
        .cloned()
        .collect();
    let probes = live.iter().map(|s| {
        let app = app.clone();
        let path = path.clone();
        async move {
            let events =
                crate::diagnosis::tail_events_within(&app.session_dir(&s.id).join("events.jsonl"), READ_TAIL_BYTES).await;
            let activity = file_activity(&events, &path);
            let diff = file_diff(&app, s, &path).await;
            (s, activity, diff)
        }
    });
    let mut colonies = Vec::new();
    for (s, activity, diff) in futures_util::future::join_all(probes).await {
        if activity.is_empty() && diff.is_none() {
            continue;
        }
        let (diff, truncated) = match diff {
            Some(d) => {
                let (d, cut) = cap_diff(d, FILE_DIFF_LIMIT);
                (Some(d), cut)
            }
            None => (None, false),
        };
        colonies.push(json!({
            "id": s.id,
            "title": if s.issue_title.is_empty() { "open session" } else { s.issue_title.as_str() },
            "issue": s.issue,
            "status": s.status.as_str(),
            "mode": if diff.is_some() { "changing" } else { "reading" },
            "activity": activity,
            "diff": diff,
            "diff_truncated": truncated,
        }));
    }
    Ok(Json(json!({"repo": repo, "path": path, "colonies": colonies})))
}

/// A relative token shaped like a repository path: at least one `/`, path characters only, not a
/// flag, a URL or a glob.
fn looks_like_repo_path(token: &str) -> bool {
    token.contains('/')
        && !token.starts_with('-')
        && !token.starts_with('/')
        && !token.contains("://")
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '@' | '+'))
}

/// `/workspace/crates/x.rs` → `crates/x.rs`; relative paths pass through; anything outside the
/// worktree (or the worktree root itself) is `None`.
fn workspace_relative(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let rel = if let Some(rest) = raw.strip_prefix("/workspace/") {
        rest
    } else if raw.starts_with('/') || raw.starts_with('~') {
        return None;
    } else {
        raw.strip_prefix("./").unwrap_or(raw)
    };
    let rel = rel.trim_end_matches('/');
    if rel.is_empty() || rel == "." || rel.contains("..") || rel.contains('*') {
        return None;
    }
    Some(rel.to_string())
}

/// `GET /api/touched`: `{sessions: {id: [path…]}}` for every live colony, cached for a few seconds
/// so a map open in several tabs does not multiply the git probes.
pub async fn touched(State(app): State<Shared>) -> Json<Value> {
    if let Ok(cache) = TOUCHED_CACHE.lock()
        && let Some((at, value)) = cache.as_ref()
        && at.elapsed() < TOUCHED_TTL
    {
        return Json(value.clone());
    }
    let live: Vec<Session> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.status.is_live() || s.status == SessionStatus::Publishing)
        .cloned()
        .collect();
    let probes = live.iter().map(|s| {
        let app = app.clone();
        async move { (s.id.clone(), touched_for(&app, s).await) }
    });
    let mut by_id = BTreeMap::new();
    for (id, files) in futures_util::future::join_all(probes).await {
        if let Some(files) = files.filter(|f| !f.is_empty()) {
            by_id.insert(id, files);
        }
    }
    // What each live colony has been looking at lately, so a colony that is still exploring (or
    // blocked) walks the chambers it reads instead of waiting at the surface.
    let reads = live.iter().map(|s| {
        let path = app.session_dir(&s.id).join("events.jsonl");
        async move {
            let events = crate::diagnosis::tail_events_within(&path, READ_TAIL_BYTES).await;
            (s.id.clone(), recent_reads(&events))
        }
    });
    let mut reading = BTreeMap::new();
    for (id, paths) in futures_util::future::join_all(reads).await {
        if !paths.is_empty() {
            reading.insert(id, paths);
        }
    }
    let value = json!({"sessions": by_id, "reading": reading});
    if let Ok(mut cache) = TOUCHED_CACHE.lock() {
        *cache = Some((Instant::now(), value.clone()));
    }
    Json(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Value {
        json!({
            "schema_version": 1,
            "diagram_type": "architecture",
            "meta": {"title": "Demo", "repository": {"revision": "abc123"}},
            "components": [
                {"id": "web", "type": "frontend", "label": "Web", "pos": [10, 20], "size": [160, 60], "sources": [{"path": "./web/src", "line": 3}]},
                {"id": "api", "type": "backend", "label": "API", "pos": [300, 20], "sources": [{"path": "crates/api/src/main.rs"}]},
                {"id": "gh", "type": "external", "label": "GitHub", "pos": [600, 20]}
            ],
            "connections": [{"from": "web", "to": "api", "label": "REST"}, {"from": "api", "to": "gh"}],
            "boundaries": [{"kind": "region", "label": "app", "wraps": ["web", "api"]}],
            "cards": [{"title": "dropped"}]
        })
    }

    #[test]
    fn a_valid_diagram_keeps_only_what_the_cockpit_draws() {
        let map = validate_map(&doc()).unwrap();
        assert_eq!(map["title"], "Demo");
        assert_eq!(map["components"].as_array().unwrap().len(), 3);
        assert_eq!(
            map["components"][0]["sources"][0]["path"], "web/src",
            "a leading ./ is dropped"
        );
        assert_eq!(
            map["components"][1]["size"],
            json!([160.0, 60.0]),
            "a missing size gets a default"
        );
        assert_eq!(map["connections"][0]["label"], "REST");
        assert_eq!(map["boundaries"][0]["wraps"], json!(["web", "api"]));
        assert!(map.get("cards").is_none(), "fields the cockpit does not draw are not stored");
    }

    #[test]
    fn broken_diagrams_are_refused_with_a_reason() {
        let with = |f: &dyn Fn(&mut Value)| {
            let mut d = doc();
            f(&mut d);
            validate_map(&d).unwrap_err().to_string()
        };
        assert!(with(&|d| d["diagram_type"] = json!("workflow")).contains("architecture"));
        assert!(with(&|d| d["components"][1]["id"] = json!("web")).contains("twice"));
        assert!(with(&|d| d["components"][0]["id"] = json!("a b")).contains("plain id"));
        assert!(with(&|d| d["connections"][0]["to"] = json!("nope")).contains("does not exist"));
        assert!(with(&|d| d["boundaries"][0]["wraps"] = json!(["ghost"])).contains("not a component"));
        assert!(with(&|d| d["components"][0]["sources"][0]["path"] = json!("../etc/passwd")).contains("unsafe"));
        assert!(with(&|d| d["components"][0]["sources"][0]["path"] = json!("/etc/passwd")).contains("unsafe"));
        assert!(with(&|d| d["components"][0]["pos"] = json!([1])).contains("pos"));
        assert!(
            with(&|d| {
                d["components"][0]["sources"] = json!([]);
                d["components"][1]["sources"] = json!([]);
            })
            .contains("source-backed")
        );
    }

    #[test]
    fn maps_live_under_the_data_dir_per_repository_and_nowhere_else() {
        let data = FsPath::new("/data");
        assert_eq!(map_path(data, "acme/web").unwrap(), PathBuf::from("/data/maps/acme/web.json"));
        assert!(map_path(data, "../x").is_none());
        assert!(map_path(data, "acme/../../x").is_none());
        assert!(map_path(data, "acme").is_none());
    }

    #[test]
    fn touched_files_merge_uncommitted_first_without_duplicates_or_unsafe_paths() {
        assert_eq!(name_only_paths("a.rs\0b/c.ts\0\0"), ["a.rs", "b/c.ts"]);
        assert_eq!(
            merge_touched(
                vec!["a.rs".into(), "b.rs".into()],
                vec!["b.rs".into(), "c.rs".into(), "../x".into()]
            ),
            ["a.rs", "b.rs", "c.rs"]
        );
        let many: Vec<String> = (0..500).map(|i| format!("f{i}.rs")).collect();
        assert_eq!(merge_touched(many, vec![]).len(), MAX_TOUCHED);
    }

    #[test]
    fn recent_reads_are_workspace_paths_newest_first_without_duplicates() {
        let events: Vec<Value> = [
            json!({"type": "tool_call", "name": "Read", "input": {"file_path": "/workspace/crates/colonizer/src/mesh.rs"}}),
            json!({"type": "tool_result", "content": "ignored"}),
            json!({"type": "tool_call", "name": "Bash", "input": {"command": "sed -n 1,40p /workspace/web/src/App.tsx; cat /opt/colonizer/x.md"}}),
            json!({"type": "tool_call", "name": "Grep", "input": {"pattern": "fn", "path": "/workspace/crates/colonizer/src"}}),
            json!({"type": "tool_call", "name": "Read", "input": {"file_path": "/workspace/crates/colonizer/src/mesh.rs"}}),
            json!({"type": "tool_call", "name": "Glob", "input": {"pattern": "**/*.rs", "path": "/workspace"}}),
            json!({"type": "tool_call", "name": "Bash", "input": {"command": "cd /workspace && grep -n route crates/colonizer/src/main.rs -A3"}}),
        ]
        .into();
        assert_eq!(
            recent_reads(&events),
            vec![
                "crates/colonizer/src/main.rs",
                "crates/colonizer/src/mesh.rs",
                "crates/colonizer/src",
                "web/src/App.tsx"
            ],
            "newest first, deduplicated, the worktree root and paths outside it dropped"
        );
        assert!(recent_reads(&[]).is_empty());
    }

    #[test]
    fn file_queries_are_repository_relative_and_never_climb_out() {
        assert_eq!(
            valid_file_query("crates/x/src/gateway.rs").as_deref(),
            Some("crates/x/src/gateway.rs")
        );
        assert_eq!(valid_file_query("./web/src/App.tsx").as_deref(), Some("web/src/App.tsx"));
        for bad in [
            "",
            "/etc/passwd",
            "../secret",
            "a/../../b",
            "a//b",
            "a/./b",
            "a\\b",
            "a\u{0}b",
        ] {
            assert_eq!(valid_file_query(bad), None, "{bad:?}");
        }
        assert_eq!(valid_file_query(&"a/".repeat(600)), None, "longer than 1024 characters");
    }

    #[test]
    fn file_activity_lists_the_calls_on_that_file_newest_first_with_their_settler() {
        let call = |ts: &str, name: &str, input: Value| json!({"type": "tool_call", "ts": ts, "name": name, "input": input, "agent": {"description": "Hunt gateway injection", "name": "general-purpose"}});
        let events = vec![
            call(
                "t1",
                "Read",
                json!({"file_path": "/workspace/crates/c/src/gateway.rs", "offset": 120, "limit": 60}),
            ),
            call(
                "t2",
                "Grep",
                json!({"pattern": "reserve", "path": "/workspace/crates/c/src/gateway.rs"}),
            ),
            call("t3", "Read", json!({"file_path": "/workspace/crates/c/src/other.rs"})),
            call(
                "t4",
                "Edit",
                json!({"file_path": "/workspace/crates/c/src/gateway.rs", "old_string": "a\nb\nc", "new_string": "a\nb\nc\nd\ne\nf\ng"}),
            ),
            call(
                "t5",
                "Bash",
                json!({"command": "cd /workspace && cargo test -p c crates/c/src/gateway.rs"}),
            ),
            json!({"type": "tool_result", "ts": "t6"}),
        ];
        let got = file_activity(&events, "crates/c/src/gateway.rs");
        let summaries: Vec<&str> = got.iter().map(|a| a["summary"].as_str().unwrap()).collect();
        assert_eq!(
            summaries,
            vec![
                "Bash: cargo test -p c crates/c/src/gateway.rs",
                "Edit (\u{2212}3 +7)",
                "Grep \"reserve\" in gateway.rs",
                "Read gateway.rs:120-180",
            ]
        );
        assert_eq!(got[0]["ts"], "t5");
        assert_eq!(got[0]["agent"], "Hunt gateway injection");
        assert!(file_activity(&events, "crates/c/src/none.rs").is_empty());
    }

    #[test]
    fn a_long_diff_is_cut_on_a_line_and_flagged() {
        let diff = "+line one\n+line two\n+line three\n".to_string();
        assert_eq!(cap_diff(diff.clone(), 1000), (diff.clone(), false));
        let (cut, flagged) = cap_diff(diff, 15);
        assert!(flagged);
        assert_eq!(cut, "+line one\n");
        assert!(new_file_diff("a/b.rs", "x\ny\n").ends_with("@@ -0,0 +1,2 @@\n+x\n+y\n"));
    }

    #[test]
    fn a_mapping_colony_always_gets_archify_once() {
        assert_eq!(with_archify(""), "archify");
        assert_eq!(with_archify("ecc, superpowers"), "ecc,superpowers,archify");
        assert_eq!(with_archify("archify,ecc"), "archify,ecc");
    }

    #[test]
    fn the_prompt_keeps_the_repository_untouched_and_names_the_output() {
        let p = map_prompt("acme/web");
        assert!(p.contains("/harness/out/architecture.json"));
        assert!(p.contains("--repo-root /workspace"));
        assert!(p.contains("Do not change anything in /workspace"));
        assert!(p.contains("do not write\n/harness/out/pr.md") || p.contains("do not write /harness/out/pr.md"));
    }
}
