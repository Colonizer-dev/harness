//! Shared memory: approved markdown notes per scope (global, GitHub org, repository), mounted
//! read-only into colonies, plus a review queue for notes that colonies propose.
//!
//! Colonies never write memory directly: a proposal arrives as an agent event over the existing
//! colony link, and only an approved proposal becomes a note other colonies can read. That review
//! step is what keeps one colony from injecting instructions into every future colony.

use crate::{
    client_error,
    orgs::valid_org,
    util::{short_id, truncate, valid_repo},
    ApiResult, Shared,
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
            write_index(&dir, scope, key, &[])?;
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
        write_index(dir, scope, key, notes)
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
        proposals.sort_by(|a, b| b.note.created_at.cmp(&a.note.created_at));
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

/// `MEMORY.md`: the index agents read first.
fn write_index(dir: &FsPath, scope: &str, key: &str, notes: &[Note]) -> Result<()> {
    let label = match scope {
        "global" => "every colony".to_string(),
        "org" => format!("colonies in the {key} org"),
        _ => format!("colonies on {key}"),
    };
    let mut index = format!(
        "# Shared memory for {label}\n\nApproved notes from earlier colonies and the maintainer. Read the ones that matter for your task.\n\n"
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
// HTTP handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ScopeQuery {
    scope: String,
    #[serde(default)]
    key: String,
}

pub async fn get(State(app): State<Shared>, Query(query): Query<ScopeQuery>) -> ApiResult<Value> {
    let notes = app.memory.notes(&query.scope, &query.key).await.map_err(|e| client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")))?;
    let proposals: Vec<Proposal> = app
        .memory
        .proposals()
        .await
        .into_iter()
        .filter(|p| p.note.scope == query.scope && p.note.key == query.key)
        .collect();
    Ok(Json(json!({"scope": query.scope, "key": query.key, "notes": notes, "proposals": proposals})))
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
    Ok(Json(app.memory.add_note(note).await?))
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
    Ok(Json(app.memory.add_note(note).await?))
}

pub async fn delete_note(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Value> {
    let removed = app
        .memory
        .delete_note(&query.scope, &query.key, &id)
        .await
        .map_err(|e| client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")))?;
    if !removed {
        return Err(client_error(StatusCode::NOT_FOUND, "no such note"));
    }
    Ok(Json(json!({"ok": true})))
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
