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
    extract::{Path, State},
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
    let value = json!({"sessions": by_id});
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
