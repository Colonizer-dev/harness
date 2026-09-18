//! Shared memory: approved markdown notes per scope (global, GitHub org, repository), mounted
//! read-only into colonies, plus a review queue for notes that colonies propose.
//!
//! Colonies never write memory directly: a proposal arrives as an agent event over the existing
//! colony link, and only an approved proposal becomes a note other colonies can read. That review
//! step is what keeps one colony from injecting instructions into every future colony.
//!
//! Approved notes live in one of two places, picked as the memory module's provider: `files`, this
//! file's own store, or `mem0` (see `mem0.rs`). Proposals stay here either way, and a colony reads
//! the same layout either way.

use crate::{
    client_error,
    config::setting_str,
    mem0::{self, Mem0},
    modules::schema_for,
    orgs::valid_org,
    util::{env_nonempty, read_trimmed, short_id, truncate, valid_repo, write_secret},
    ApiResult, App, Shared,
};
use anyhow::{bail, Context, Result};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path as FsPath, PathBuf};
use tokio::sync::Mutex;

const MAX_TITLE: usize = 200;
const MAX_CONTENT: usize = 20_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub scope: String,
    pub key: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub source: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    #[serde(flatten)]
    pub note: Note,
    pub status: String,
}

pub struct MemoryStore {
    root: PathBuf,
    lock: Mutex<()>,
}

/// Normalizes and validates a draft note. Returns a clear message for the API on failure.
pub fn draft(scope: &str, key: &str, title: &str, content: &str, tags: &[String], source: Value) -> Result<Note> {
    match scope {
        "global" if key.is_empty() => {}
        "org" if valid_org(key) => {}
        "repo" if valid_repo(key) => {}
        _ => bail!("scope must be global (no key), org (key = org) or repo (key = owner/repo)"),
    }
    let title: String = title.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string();
    if title.is_empty() || title.chars().count() > MAX_TITLE {
        bail!("title must be 1-{MAX_TITLE} characters");
    }
    let content = content.trim();
    if content.is_empty() || content.chars().count() > MAX_CONTENT {
        bail!("content must be 1-{MAX_CONTENT} characters");
    }
    let tags: Vec<String> = tags
        .iter()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty() && t.len() <= 40 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .take(10)
        .collect();
    Ok(Note {
        id: short_id(),
        scope: scope.to_string(),
        key: key.to_string(),
        title,
        content: content.to_string(),
        tags,
        created_at: Utc::now(),
        source,
    })
}

impl MemoryStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root, lock: Mutex::new(()) }
    }

    fn scope_dir(&self, scope: &str, key: &str) -> Result<PathBuf> {
        Ok(match scope {
            "global" => self.root.join("global"),
            "org" if valid_org(key) => self.root.join("orgs").join(key),
            "repo" if valid_repo(key) => self.root.join("repos").join(key),
            _ => bail!("invalid memory scope"),
        })
    }

    /// Creates a scope's directory and index so it can be mounted into a colony.
    pub fn ensure_scope(&self, scope: &str, key: &str) -> Result<PathBuf> {
        let dir = self.scope_dir(scope, key)?;
        std::fs::create_dir_all(dir.join("notes"))?;
        if !dir.join("MEMORY.md").exists() {
            write_index(&dir, scope, key, &[], false)?;
        }
        Ok(dir)
    }

    fn read_notes(dir: &FsPath) -> Vec<Note> {
        std::fs::read(dir.join("notes.json")).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
    }

    fn write_notes(dir: &FsPath, scope: &str, key: &str, notes: &[Note]) -> Result<()> {
        let tmp = dir.join("notes.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(notes)?)?;
        std::fs::rename(&tmp, dir.join("notes.json"))?;
        write_index(dir, scope, key, notes, false)
    }

    pub async fn notes(&self, scope: &str, key: &str) -> Result<Vec<Note>> {
        let _guard = self.lock.lock().await;
        Ok(Self::read_notes(&self.scope_dir(scope, key)?))
    }

    pub async fn add_note(&self, note: Note) -> Result<Note> {
        let _guard = self.lock.lock().await;
        let dir = self.ensure_scope(&note.scope, &note.key)?;
        std::fs::write(dir.join("notes").join(format!("{}.md", note.id)), format!("# {}\n\n{}\n", note.title, note.content))?;
        let mut notes = Self::read_notes(&dir);
        notes.push(note.clone());
        Self::write_notes(&dir, &note.scope, &note.key, &notes)?;
        Ok(note)
    }

    pub async fn delete_note(&self, scope: &str, key: &str, id: &str) -> Result<bool> {
        let _guard = self.lock.lock().await;
        let dir = self.scope_dir(scope, key)?;
        let mut notes = Self::read_notes(&dir);
        let before = notes.len();
        notes.retain(|n| n.id != id);
        if notes.len() == before {
            return Ok(false);
        }
        if id.chars().all(|c| c.is_ascii_alphanumeric()) {
            let _ = std::fs::remove_file(dir.join("notes").join(format!("{id}.md")));
        }
        Self::write_notes(&dir, scope, key, &notes)?;
        Ok(true)
    }

    fn proposals_path(&self) -> PathBuf {
        self.root.join("proposals.json")
    }

    fn read_proposals(&self) -> Vec<Proposal> {
        std::fs::read(self.proposals_path()).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
    }

    fn write_proposals(&self, proposals: &[Proposal]) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let tmp = self.root.join("proposals.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(proposals)?)?;
        std::fs::rename(&tmp, self.proposals_path())?;
        Ok(())
    }

    /// Pending proposals, newest first.
    pub async fn proposals(&self) -> Vec<Proposal> {
        let _guard = self.lock.lock().await;
        let mut proposals = self.read_proposals();
        proposals.sort_by_key(|a| std::cmp::Reverse(a.note.created_at));
        proposals
    }

    pub async fn add_proposal(&self, note: Note) -> Result<Proposal> {
        let _guard = self.lock.lock().await;
        let mut proposals = self.read_proposals();
        if proposals.len() >= 500 {
            bail!("too many pending memory proposals; review some first");
        }
        let proposal = Proposal { note, status: "pending".into() };
        proposals.push(proposal.clone());
        self.write_proposals(&proposals)?;
        Ok(proposal)
    }

    pub async fn take_proposal(&self, id: &str) -> Result<Option<Proposal>> {
        let _guard = self.lock.lock().await;
        let mut proposals = self.read_proposals();
        let Some(index) = proposals.iter().position(|p| p.note.id == id) else { return Ok(None) };
        let proposal = proposals.remove(index);
        self.write_proposals(&proposals)?;
        Ok(Some(proposal))
    }
}

/// `MEMORY.md`: the index agents read first. `ranked` says the notes arrive most relevant first.
fn write_index(dir: &FsPath, scope: &str, key: &str, notes: &[Note], ranked: bool) -> Result<()> {
    let label = match scope {
        "global" => "every colony".to_string(),
        "org" => format!("colonies in the {key} org"),
        _ => format!("colonies on {key}"),
    };
    let order = if ranked { " They are listed most relevant to this colony's task first." } else { "" };
    let mut index = format!(
        "# Shared memory for {label}\n\nApproved notes from earlier colonies and the maintainer. Read the ones that matter for your task.{order}\n\n"
    );
    if notes.is_empty() {
        index.push_str("No notes yet.\n");
    }
    for note in notes {
        let first_line = note.content.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
        let title = note.title.replace(['[', ']'], "");
        index.push_str(&format!("- [{title}](notes/{}.md) — {}\n", note.id, truncate(first_line.trim(), 120)));
    }
    std::fs::write(dir.join("MEMORY.md"), index).context("writing MEMORY.md")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Where approved notes live
// ---------------------------------------------------------------------------

pub const MEM0: &str = "mem0";

fn mem0_key_file(app: &App) -> PathBuf {
    app.cfg.config_dir.join("memory-keys").join(MEM0)
}

/// The saved mem0 key, else `MEM0_API_KEY`. Lives beside the model provider keys and, like them,
/// is never written to modules.json, never returned by the API and never sent into a colony.
fn mem0_key(app: &App) -> Option<(String, &'static str)> {
    read_trimmed(&mem0_key_file(app))
        .map(|key| (key, "saved"))
        .or_else(|| env_nonempty("MEM0_API_KEY").map(|key| (key, "MEM0_API_KEY")))
}

pub async fn uses_mem0(app: &App) -> bool {
    app.modules.read().await.memory.provider == MEM0
}

async fn mem0_client(app: &App) -> Result<Mem0> {
    let base_url = {
        let modules = app.modules.read().await;
        setting_str(&modules.memory, &schema_for("memory", MEM0, &app.agents), "base_url")
    };
    let (key, _) = mem0_key(app).context("add a mem0 API key in Settings → Modules → Memory")?;
    Mem0::new(if base_url.is_empty() { mem0::DEFAULT_BASE_URL } else { &base_url }, key)
}

pub async fn list_notes(app: &App, scope: &str, key: &str) -> Result<Vec<Note>> {
    if uses_mem0(app).await {
        mem0::scope_id(scope, key)?;
        return mem0_client(app).await?.list(scope, key).await;
    }
    app.memory.notes(scope, key).await
}

/// Stores an approved note in whichever place the memory module points at.
pub async fn store_note(app: &App, mut note: Note) -> Result<Note> {
    if uses_mem0(app).await {
        note.id = mem0_client(app).await?.add(&note).await?;
        return Ok(note);
    }
    app.memory.add_note(note).await
}

pub async fn remove_note(app: &App, scope: &str, key: &str, id: &str) -> Result<bool> {
    if uses_mem0(app).await {
        return mem0_client(app).await?.delete(scope, key, id).await;
    }
    app.memory.delete_note(scope, key, id).await
}

/// What a colony is for, as a relevance query: the issue and the instructions, never the prompt
/// around them. That prompt is mostly harness boilerplate identical for every colony, which would
/// pull every ranking toward the same notes, and it puts the task far enough in that a long preamble
/// could push it past the query length mem0 is sent.
pub fn task_query(title: &str, issue: Option<&Value>, instructions: &str) -> String {
    let text = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    let issue_title = issue.map(|i| text(&i["title"])).unwrap_or_default();
    let issue_body = issue.map(|i| text(&i["body"])).unwrap_or_default();
    let title = if issue_title.is_empty() { title.trim().to_string() } else { issue_title };
    [title, instructions.trim().to_string(), issue_body].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join("\n\n")
}

pub struct Materialized {
    pub notes: usize,
    pub ranked: bool,
}

/// Writes a colony's shared memory from mem0 into `root/{global,org,repo}`, in exactly the layout
/// the `files` provider mounts: `MEMORY.md` plus `notes/<id>.md`. `root` is inside the colony's
/// read-only session directory, so this is the whole of what the colony sees — mem0 itself stays
/// on this side, with the key.
///
/// Relevance to `task` orders each index. That ranking is a nicety, so a failed search keeps the
/// oldest-first order rather than costing the colony its memory.
pub async fn materialize_mem0(app: &App, root: &FsPath, org: &str, repo: &str, task: &str) -> Result<Materialized> {
    materialize(&mem0_client(app).await?, root, org, repo, task).await
}

async fn materialize(client: &Mem0, root: &FsPath, org: &str, repo: &str, task: &str) -> Result<Materialized> {
    let scopes = [("global", ""), ("org", org), ("repo", repo)];
    let mut listed = Vec::new();
    for (scope, key) in scopes {
        listed.push(client.list(scope, key).await?);
    }
    let total: usize = listed.iter().map(Vec::len).sum();
    let ranks = if total == 0 || task.trim().is_empty() { None } else { client.rank(task, &scopes, total.min(100)).await.ok() };
    for ((scope, key), mut notes) in scopes.into_iter().zip(listed) {
        if let Some(ranks) = &ranks {
            // Stable: notes mem0 did not rank keep their oldest-first order after the ranked ones.
            notes.sort_by_key(|n| ranks.get(&n.id).copied().unwrap_or(usize::MAX));
        }
        write_scope(&root.join(scope), scope, key, &notes, ranks.is_some())?;
    }
    Ok(Materialized { notes: total, ranked: ranks.is_some() })
}

/// Replaces one scope directory with `notes`. A note whose id could not be a file name is left out
/// rather than written somewhere unexpected.
fn write_scope(dir: &FsPath, scope: &str, key: &str, notes: &[Note], ranked: bool) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir.join("notes"))?;
    let notes: Vec<Note> = notes.iter().filter(|n| mem0::safe_id(&n.id)).cloned().collect();
    for note in &notes {
        std::fs::write(dir.join("notes").join(format!("{}.md", note.id)), format!("# {}\n\n{}\n", note.title, note.content))?;
    }
    write_index(dir, scope, key, &notes, ranked)
}

/// An empty, valid layout, for a colony whose memory could not be fetched: the prompt tells the
/// agent to read each `MEMORY.md`, and a missing file would read as a broken colony.
pub fn write_empty_scopes(root: &FsPath, org: &str, repo: &str) -> Result<()> {
    for (scope, key) in [("global", ""), ("org", org), ("repo", repo)] {
        write_scope(&root.join(scope), scope, key, &[], false)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ScopeQuery {
    scope: String,
    #[serde(default)]
    key: String,
}

/// A note store that could not answer: a bad scope is the caller's fault, anything else (mem0 down,
/// a rejected key) is upstream's.
fn store_error(scope: &str, key: &str, e: &anyhow::Error) -> crate::AppError {
    let status = if mem0::scope_id(scope, key).is_err() { StatusCode::BAD_REQUEST } else { StatusCode::BAD_GATEWAY };
    client_error(status, &format!("{e:#}"))
}

pub async fn get(State(app): State<Shared>, Query(query): Query<ScopeQuery>) -> ApiResult<Value> {
    let notes = list_notes(&app, &query.scope, &query.key).await.map_err(|e| store_error(&query.scope, &query.key, &e))?;
    let proposals: Vec<Proposal> = app
        .memory
        .proposals()
        .await
        .into_iter()
        .filter(|p| p.note.scope == query.scope && p.note.key == query.key)
        .collect();
    let provider = app.modules.read().await.memory.provider.clone();
    Ok(Json(json!({"scope": query.scope, "key": query.key, "provider": provider, "notes": notes, "proposals": proposals})))
}

pub async fn list_proposals(State(app): State<Shared>) -> Json<Vec<Proposal>> {
    Json(app.memory.proposals().await)
}

#[derive(Deserialize, Default)]
struct Edits {
    title: Option<String>,
    content: Option<String>,
}

pub async fn approve(State(app): State<Shared>, Path(id): Path<String>, body: Bytes) -> ApiResult<Note> {
    let edits: Edits = if body.is_empty() {
        Edits::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| client_error(StatusCode::BAD_REQUEST, "expected {title?, content?}"))?
    };
    let proposal = app.memory.take_proposal(&id).await?.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such proposal"))?;
    let original = proposal.note.clone();
    let note = draft(
        &original.scope,
        &original.key,
        edits.title.as_deref().unwrap_or(&original.title),
        edits.content.as_deref().unwrap_or(&original.content),
        &original.tags,
        original.source.clone(),
    );
    let note = match note {
        Ok(mut note) => {
            note.id = original.id.clone();
            note
        }
        Err(e) => {
            // Put the proposal back so an invalid edit doesn't lose it.
            let _ = app.memory.add_proposal(original).await;
            return Err(client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")));
        }
    };
    match store_note(&app, note).await {
        Ok(stored) => Ok(Json(stored)),
        Err(e) => {
            // Same reason: mem0 being down or refusing the key must not cost a reviewed proposal.
            let _ = app.memory.add_proposal(original).await;
            Err(client_error(StatusCode::BAD_GATEWAY, &format!("{e:#}")))
        }
    }
}

pub async fn reject(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    app.memory.take_proposal(&id).await?.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such proposal"))?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct NewNote {
    scope: String,
    #[serde(default)]
    key: String,
    title: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

pub async fn create_note(State(app): State<Shared>, Json(req): Json<NewNote>) -> ApiResult<Note> {
    let note = draft(&req.scope, &req.key, &req.title, &req.content, &req.tags, json!({"user": true}))
        .map_err(|e| client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")))?;
    Ok(Json(store_note(&app, note).await.map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("{e:#}")))?))
}

pub async fn delete_note(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Value> {
    let removed = remove_note(&app, &query.scope, &query.key, &id).await.map_err(|e| store_error(&query.scope, &query.key, &e))?;
    if !removed {
        return Err(client_error(StatusCode::NOT_FOUND, "no such note"));
    }
    Ok(Json(json!({"ok": true})))
}

/// Whether a mem0 key is set, and where from. Never the key.
pub async fn mem0_status(State(app): State<Shared>) -> Json<Value> {
    let source = mem0_key(&app).map(|(_, source)| source);
    Json(json!({"has_key": source.is_some(), "source": source, "active": uses_mem0(&app).await}))
}

#[derive(Deserialize)]
pub struct Mem0Key {
    api_key: String,
}

/// Saves the mem0 key on this machine, or removes it when empty.
pub async fn put_mem0_key(State(app): State<Shared>, Json(req): Json<Mem0Key>) -> ApiResult<Value> {
    let key = req.api_key.trim();
    let path = mem0_key_file(&app);
    if key.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else if key.len() > 512 || !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err(client_error(StatusCode::BAD_REQUEST, "that doesn't look like a mem0 API key"));
    } else {
        write_secret(&path, key)?;
    }
    Ok(mem0_status(State(app)).await)
}

/// Tries the saved key against the configured endpoint.
pub async fn check_mem0(State(app): State<Shared>) -> Json<Value> {
    let result = match mem0_client(&app).await {
        Ok(client) => client.check().await,
        Err(e) => Err(e),
    };
    Json(match result {
        Ok(()) => json!({"ok": true}),
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-memory-test-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn proposals_become_notes_with_an_index() {
        let root = temp_root();
        let store = MemoryStore::new(root.clone());
        let note = draft("repo", "Colonizer-dev/harness", "Run tests\nwith --locked", "Use `cargo test --locked`.", &["Tests".into()], json!({"session_id": "abc"})).unwrap();
        assert_eq!(note.title, "Run tests with --locked");
        assert_eq!(note.tags, vec!["tests"]);

        let proposal = store.add_proposal(note).await.unwrap();
        assert_eq!(store.proposals().await.len(), 1);
        let taken = store.take_proposal(&proposal.note.id).await.unwrap().unwrap();
        store.add_note(taken.note.clone()).await.unwrap();
        assert!(store.proposals().await.is_empty());

        let dir = root.join("repos/Colonizer-dev/harness");
        let index = std::fs::read_to_string(dir.join("MEMORY.md")).unwrap();
        assert!(index.contains(&format!("[Run tests with --locked](notes/{}.md)", taken.note.id)));
        assert!(dir.join("notes").join(format!("{}.md", taken.note.id)).exists());

        assert!(store.delete_note("repo", "Colonizer-dev/harness", &taken.note.id).await.unwrap());
        assert!(store.notes("repo", "Colonizer-dev/harness").await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_colony_gets_mem0_notes_in_the_files_layout_most_relevant_first() {
        let mock = crate::mem0::mock::Mock::default();
        let base = crate::mem0::mock::serve(mock.clone()).await;
        let client = Mem0::new(&base, crate::mem0::mock::KEY.into()).unwrap();
        let older = client.add(&draft("repo", "o/r", "Commit style", "keep commits small", &[], Value::Null).unwrap()).await.unwrap();
        let newer = client.add(&draft("repo", "o/r", "Deploys", "deploy to staging before production", &[], Value::Null).unwrap()).await.unwrap();
        client.add(&draft("org", "o", "Org rule", "sign every commit", &[], Value::Null).unwrap()).await.unwrap();
        client.add(&draft("repo", "o/elsewhere", "Not this repo", "deploy something else entirely", &[], Value::Null).unwrap()).await.unwrap();

        let root = temp_root();
        let result = materialize(&client, &root, "o", "o/r", "Fix the staging deploy").await.unwrap();
        assert_eq!(result.notes, 3, "global, this org and this repo; not another repo");
        assert!(result.ranked);

        let index = std::fs::read_to_string(root.join("repo/MEMORY.md")).unwrap();
        assert!(index.contains("most relevant to this colony's task first"));
        let deploys = index.find(&format!("(notes/{newer}.md)")).unwrap();
        let commits = index.find(&format!("(notes/{older}.md)")).unwrap();
        assert!(deploys < commits, "the deploy note is the relevant one:\n{index}");
        assert_eq!(std::fs::read_to_string(root.join(format!("repo/notes/{newer}.md"))).unwrap(), "# Deploys\n\ndeploy to staging before production\n");
        assert!(std::fs::read_to_string(root.join("org/MEMORY.md")).unwrap().contains("Org rule"));
        assert!(std::fs::read_to_string(root.join("global/MEMORY.md")).unwrap().contains("No notes yet."));

        // A resume rewrites the scope rather than leaving a deleted note behind.
        assert!(client.delete("repo", "o/r", &older).await.unwrap());
        materialize(&client, &root, "o", "o/r", "Fix the staging deploy").await.unwrap();
        assert!(!root.join(format!("repo/notes/{older}.md")).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_relevance_query_is_the_task_not_the_prompt() {
        let issue = json!({"title": "Fix the staging deploy", "body": "It fails on the migrate step."});
        let query = task_query("ignored when there is an issue", Some(&issue), "Keep the diff small.");
        assert_eq!(query, "Fix the staging deploy\n\nKeep the diff small.\n\nIt fails on the migrate step.");
        // Instructions come before the body: a long issue body is what gets cut, not what was asked.
        assert_eq!(task_query("Tidy the README", None, ""), "Tidy the README");
        assert_eq!(task_query("", None, ""), "");
    }

    #[test]
    fn scopes_and_sizes_are_validated() {
        assert!(draft("global", "", "t", "c", &[], Value::Null).is_ok());
        assert!(draft("global", "x", "t", "c", &[], Value::Null).is_err());
        assert!(draft("org", "../x", "t", "c", &[], Value::Null).is_err());
        assert!(draft("repo", "owner", "t", "c", &[], Value::Null).is_err());
        assert!(draft("repo", "o/r", "", "c", &[], Value::Null).is_err());
        assert!(draft("repo", "o/r", "t", &"x".repeat(MAX_CONTENT + 1), &[], Value::Null).is_err());
    }
}
