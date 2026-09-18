//! mem0 (https://mem0.ai) as the store behind shared memory, through its Platform API (v3).
//!
//! The `files` provider's guarantees carry over unchanged, and this module is shaped around keeping
//! them rather than around mem0's own model:
//!
//! - **Review stays on the Mothership.** Proposals queue locally exactly as before. mem0 only ever
//!   receives a note a human approved, or one stored with review switched off.
//! - **Colonies never reach mem0 and never see the key.** At boot the Mothership writes a colony's
//!   memories into the read-only layout `files` mounts, so `memory_search` and the prompt that
//!   points the agent at `MEMORY.md` work without a line changing inside the colony.
//!
//! What mem0 adds is where the notes live — in your mem0 project, readable by anything else that
//! uses it — and an index that lists the notes most relevant to a colony's task first.
//!
//! Every note is written with `infer: false` and `immutable: true`. With inference on, mem0's
//! extraction model rewrites what it stores and later consolidates it with other memories, so a
//! colony could end up reading text no human approved: the one thing review exists to prevent.

use crate::{
    memory::Note,
    orgs::valid_org,
    util::{truncate, valid_repo},
};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::{collections::HashMap, time::Duration};

pub const DEFAULT_BASE_URL: &str = "https://api.mem0.ai";
/// Every memory Colonizer writes carries this `app_id`. A mem0 project shared with other tools
/// keeps them apart, and nothing here can list or delete a memory it did not write.
pub const APP_ID: &str = "colonizer";
/// Curated, reviewed notes stay small; a scope past this is listed up to the cap.
const MAX_PER_SCOPE: usize = 500;
const PAGE_SIZE: usize = 100;
/// A task description is a search query here, not a document, and mem0 does not need all of it.
const MAX_QUERY: usize = 2_000;

pub struct Mem0 {
    base_url: String,
    key: String,
    client: reqwest::Client,
}

/// The mem0 `user_id` a Colonizer scope maps to. mem0 requires an entity id on every memory and
/// every query; a scope is the natural one, and filtering on it is what keeps one org's memory out
/// of another org's colony.
pub fn scope_id(scope: &str, key: &str) -> Result<String> {
    Ok(match scope {
        "global" if key.is_empty() => "colonizer:global".to_string(),
        "org" if valid_org(key) => format!("colonizer:org:{key}"),
        "repo" if valid_repo(key) => format!("colonizer:repo:{key}"),
        _ => bail!("invalid memory scope"),
    })
}

/// A memory id as it may appear in a file name and in a URL path. mem0 ids are UUIDs; anything
/// else is refused rather than escaped.
pub fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

impl Mem0 {
    pub fn new(base_url: &str, key: String) -> Result<Self> {
        let base_url = base_url.trim().trim_end_matches('/');
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            bail!("the mem0 base URL must start with https://");
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            base_url: base_url.to_string(),
            key,
            client,
        })
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<(StatusCode, Value)> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base_url))
            .header("Authorization", format!("Token {}", self.key));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("mem0 is unreachable")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            bail!("mem0 rejected the API key ({status})");
        }
        let value = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok((status, value))
    }

    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, value) = self.call(method, path, body).await?;
        if !status.is_success() {
            bail!("mem0 returned {status}: {}", truncate(&value.to_string(), 300));
        }
        Ok(value)
    }

    /// Confirms the key and endpoint work, with the cheapest request that needs both.
    pub async fn check(&self) -> Result<()> {
        let body = json!({"filters": {"user_id": scope_id("global", "")?, "app_id": APP_ID}});
        self.ok(Method::POST, "/v3/memories/?page=1&page_size=1", Some(body))
            .await
            .map(|_| ())
    }

    /// Stores an approved note verbatim and returns mem0's id for it.
    pub async fn add(&self, note: &Note) -> Result<String> {
        let body = json!({
            "messages": [{"role": "user", "content": format!("{}\n\n{}", note.title, note.content)}],
            "user_id": scope_id(&note.scope, &note.key)?,
            "app_id": APP_ID,
            "metadata": {
                "colonizer_id": note.id,
                "scope": note.scope,
                "key": note.key,
                "title": note.title,
                "tags": note.tags,
                "source": note.source,
                "created_at": note.created_at,
            },
            "infer": false,
            "immutable": true,
        });
        let response = self.ok(Method::POST, "/v3/memories/add/", Some(body)).await?;
        // With `infer: false` mem0 stores synchronously and answers with what it stored. No
        // results means the call went through the extraction pipeline instead, which would rewrite
        // a reviewed note — so that is an error, not a success without an id.
        response["results"]
            .as_array()
            .and_then(|results| results.first())
            .and_then(|stored| stored["id"].as_str())
            .map(str::to_string)
            .context("mem0 did not store the note verbatim (no results for infer: false)")
    }

    /// Every note in one scope, oldest first.
    pub async fn list(&self, scope: &str, key: &str) -> Result<Vec<Note>> {
        let body = json!({"filters": {"user_id": scope_id(scope, key)?, "app_id": APP_ID}});
        let mut notes = Vec::new();
        for page in 1.. {
            let path = format!("/v3/memories/?page={page}&page_size={PAGE_SIZE}");
            let response = self.ok(Method::POST, &path, Some(body.clone())).await?;
            let results = response["results"].as_array().cloned().unwrap_or_default();
            notes.extend(
                results
                    .iter()
                    .filter_map(note_from)
                    .filter(|n| n.scope == scope && n.key == key),
            );
            if results.is_empty() || response["next"].is_null() || notes.len() >= MAX_PER_SCOPE {
                break;
            }
        }
        notes.truncate(MAX_PER_SCOPE);
        notes.sort_by_key(|n| n.created_at);
        Ok(notes)
    }

    /// mem0's relevance ranking for `query` across the given scopes: memory id → rank, 0 best.
    pub async fn rank(&self, query: &str, scopes: &[(&str, &str)], top_k: usize) -> Result<HashMap<String, usize>> {
        let ids = scopes
            .iter()
            .map(|(scope, key)| scope_id(scope, key))
            .collect::<Result<Vec<_>>>()?;
        let body = json!({
            "query": truncate(query.trim(), MAX_QUERY),
            "filters": {"user_id": {"in": ids}, "app_id": APP_ID},
            "top_k": top_k.max(1),
            "threshold": 0.0,
        });
        let response = self.ok(Method::POST, "/v3/memories/search/", Some(body)).await?;
        Ok(response["results"]
            .as_array()
            .map(|results| {
                results
                    .iter()
                    .filter_map(|m| m["id"].as_str())
                    .enumerate()
                    .map(|(rank, id)| (id.to_string(), rank))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Deletes a note, but only one Colonizer wrote into this scope. A mem0 project can hold other
    /// tools' memories, and an id from the web UI is not proof of either.
    pub async fn delete(&self, scope: &str, key: &str, id: &str) -> Result<bool> {
        if !safe_id(id) {
            return Ok(false);
        }
        let expected = scope_id(scope, key)?;
        let (status, memory) = self.call(Method::GET, &format!("/v1/memories/{id}/"), None).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !status.is_success() {
            bail!("mem0 returned {status}: {}", truncate(&memory.to_string(), 300));
        }
        if memory["user_id"].as_str() != Some(expected.as_str()) || memory["app_id"].as_str() != Some(APP_ID) {
            return Ok(false);
        }
        self.ok(Method::DELETE, &format!("/v1/memories/{id}/"), None).await?;
        Ok(true)
    }
}

/// A mem0 memory back as a note. Colonizer's own fields ride in `metadata`; the stored text is
/// `title\n\ncontent`, so the title is searchable, and it is split off again here.
fn note_from(memory: &Value) -> Option<Note> {
    let id = memory["id"].as_str()?;
    let meta = &memory["metadata"];
    let scope = meta["scope"].as_str()?;
    let title = meta["title"].as_str().unwrap_or_default();
    let text = memory["memory"].as_str().unwrap_or_default();
    let content = text.strip_prefix(&format!("{title}\n\n")).unwrap_or(text);
    let created_at = [&meta["created_at"], &memory["created_at"]]
        .iter()
        .find_map(|v| v.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()))
        .map_or_else(Utc::now, |t| t.with_timezone(&Utc));
    Some(Note {
        id: id.to_string(),
        scope: scope.to_string(),
        key: meta["key"].as_str().unwrap_or_default().to_string(),
        title: title.to_string(),
        content: content.to_string(),
        tags: meta["tags"]
            .as_array()
            .map(|t| t.iter().filter_map(|t| t.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        created_at,
        source: meta["source"].clone(),
    })
}

#[cfg(test)]
pub mod mock {
    //! A mem0 Platform stand-in for tests: the four v3/v1 endpoints Colonizer calls, with the
    //! request checks that matter — the key, `infer: false`, `immutable: true`, and entity ids
    //! inside `filters` rather than beside them, which the real API rejects with a 400.

    use axum::{
        Json, Router,
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        routing::{get, post},
    };
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    pub const KEY: &str = "m0-test-key";

    #[derive(Clone, Default)]
    pub struct Mock {
        pub memories: Arc<Mutex<Vec<Value>>>,
        pub adds: Arc<Mutex<Vec<Value>>>,
    }

    fn authorized(headers: &HeaderMap) -> bool {
        headers.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Token {KEY}"))
    }

    fn matches(memory: &Value, filters: &Value) -> bool {
        let user = memory["user_id"].as_str().unwrap_or_default();
        let user_ok = match &filters["user_id"] {
            Value::String(id) => id == user,
            Value::Object(op) => op["in"].as_array().is_some_and(|ids| ids.iter().any(|id| id == user)),
            _ => false,
        };
        user_ok && memory["app_id"] == filters["app_id"]
    }

    async fn add(State(mock): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, Json(json!({"detail": "Invalid API key"})));
        }
        mock.adds.lock().unwrap().push(body.clone());
        let id = uuid::Uuid::new_v4().to_string();
        let memory = json!({
            "id": id,
            "memory": body["messages"][0]["content"],
            "user_id": body["user_id"],
            "app_id": body["app_id"],
            "metadata": body["metadata"],
            "created_at": "2026-09-18T00:00:00Z",
        });
        mock.memories.lock().unwrap().push(memory);
        // The real API answers with results only when infer is false; mirror that.
        let results = if body["infer"] == json!(false) {
            json!([{"id": id, "event": "ADD"}])
        } else {
            Value::Null
        };
        (StatusCode::OK, Json(json!({"status": "ok", "results": results})))
    }

    async fn list(State(mock): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, Json(json!({})));
        }
        if body.get("user_id").is_some() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "entity ids belong in filters"})),
            );
        }
        let results: Vec<Value> = mock
            .memories
            .lock()
            .unwrap()
            .iter()
            .filter(|m| matches(m, &body["filters"]))
            .cloned()
            .collect();
        (
            StatusCode::OK,
            Json(json!({"count": results.len(), "next": null, "previous": null, "results": results})),
        )
    }

    /// Ranks by how many query words a memory contains: crude, but deterministic.
    async fn search(State(mock): State<Mock>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, Json(json!({})));
        }
        let words: Vec<String> = body["query"]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();
        let mut scored: Vec<(usize, Value)> = mock
            .memories
            .lock()
            .unwrap()
            .iter()
            .filter(|m| matches(m, &body["filters"]))
            .map(|m| {
                let text = m["memory"].as_str().unwrap_or_default().to_lowercase();
                (words.iter().filter(|w| text.contains(w.as_str())).count(), m.clone())
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        let top_k = body["top_k"].as_u64().unwrap_or(10) as usize;
        let results: Vec<Value> = scored.into_iter().take(top_k).map(|(_, m)| m).collect();
        (StatusCode::OK, Json(json!({"results": results})))
    }

    async fn get_one(State(mock): State<Mock>, headers: HeaderMap, Path(id): Path<String>) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, Json(json!({})));
        }
        match mock.memories.lock().unwrap().iter().find(|m| m["id"] == id.as_str()) {
            Some(memory) => (StatusCode::OK, Json(memory.clone())),
            None => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))),
        }
    }

    async fn delete_one(State(mock): State<Mock>, headers: HeaderMap, Path(id): Path<String>) -> (StatusCode, Json<Value>) {
        if !authorized(&headers) {
            return (StatusCode::UNAUTHORIZED, Json(json!({})));
        }
        mock.memories.lock().unwrap().retain(|m| m["id"] != id.as_str());
        (StatusCode::OK, Json(json!({"message": "Memory deleted successfully!"})))
    }

    /// Serves the mock on a loopback port and returns its base URL.
    pub async fn serve(mock: Mock) -> String {
        let router = Router::new()
            .route("/v3/memories/add/", post(add))
            .route("/v3/memories/", post(list))
            .route("/v3/memories/search/", post(search))
            .route("/v1/memories/{id}/", get(get_one).delete(delete_one))
            .with_state(mock);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::draft;

    async fn client() -> (Mem0, mock::Mock) {
        let mock = mock::Mock::default();
        let base = mock::serve(mock.clone()).await;
        (Mem0::new(&base, mock::KEY.into()).unwrap(), mock)
    }

    #[test]
    fn scopes_map_to_entity_ids_and_bad_ones_are_refused() {
        assert_eq!(scope_id("global", "").unwrap(), "colonizer:global");
        assert_eq!(scope_id("org", "Colonizer-dev").unwrap(), "colonizer:org:Colonizer-dev");
        assert_eq!(
            scope_id("repo", "Colonizer-dev/harness").unwrap(),
            "colonizer:repo:Colonizer-dev/harness"
        );
        assert!(scope_id("global", "x").is_err());
        assert!(scope_id("org", "../x").is_err());
        assert!(scope_id("repo", "owner").is_err());
        assert!(safe_id("0b1c3e7a-9f2d-4c1a-8e5b-7d6f5a4b3c2d"));
        assert!(!safe_id("../../v1/memories"));
        assert!(!safe_id(""));
    }

    #[tokio::test]
    async fn notes_are_stored_verbatim_immutable_and_come_back_intact() {
        let (mem0, mock) = client().await;
        let note = draft(
            "repo",
            "Colonizer-dev/harness",
            "Run tests locked",
            "Use `cargo test --locked`.\n\nA second paragraph survives too.",
            &["tests".into()],
            json!({"session_id": "abc"}),
        )
        .unwrap();
        let id = mem0.add(&note).await.unwrap();

        let sent = mock.adds.lock().unwrap()[0].clone();
        assert_eq!(sent["infer"], json!(false), "inference would rewrite a reviewed note");
        assert_eq!(sent["immutable"], json!(true), "consolidation would rewrite it later");
        assert_eq!(sent["app_id"], APP_ID);
        assert_eq!(sent["user_id"], "colonizer:repo:Colonizer-dev/harness");

        let notes = mem0.list("repo", "Colonizer-dev/harness").await.unwrap();
        assert_eq!(notes.len(), 1);
        let back = &notes[0];
        assert_eq!(back.id, id);
        assert_eq!(back.title, note.title);
        assert_eq!(back.content, note.content);
        assert_eq!(back.tags, note.tags);
        assert_eq!(back.source, note.source);
        assert_eq!(back.created_at, note.created_at);
    }

    #[tokio::test]
    async fn a_scope_lists_only_its_own_notes_and_never_another_apps() {
        let (mem0, mock) = client().await;
        for (scope, key, title) in [
            ("repo", "o/r", "repo note"),
            ("org", "o", "org note"),
            ("repo", "o/other", "other repo"),
        ] {
            mem0.add(&draft(scope, key, title, "content", &[], Value::Null).unwrap())
                .await
                .unwrap();
        }
        // Another tool's memory in the same mem0 project, under the very same user_id.
        mock.memories.lock().unwrap().push(json!({
            "id": "foreign", "memory": "not ours", "user_id": "colonizer:repo:o/r", "app_id": "someone-else",
            "metadata": {"scope": "repo", "key": "o/r", "title": "foreign"},
        }));
        let titles: Vec<String> = mem0.list("repo", "o/r").await.unwrap().into_iter().map(|n| n.title).collect();
        assert_eq!(titles, vec!["repo note"]);
    }

    #[tokio::test]
    async fn delete_refuses_a_memory_colonizer_did_not_write_in_that_scope() {
        let (mem0, mock) = client().await;
        let ours = mem0
            .add(&draft("repo", "o/r", "ours", "c", &[], Value::Null).unwrap())
            .await
            .unwrap();
        mock.memories.lock().unwrap().push(
            json!({"id": "11111111-2222-3333-4444-555555555555", "user_id": "colonizer:repo:o/r", "app_id": "someone-else"}),
        );

        assert!(
            !mem0
                .delete("repo", "o/r", "11111111-2222-3333-4444-555555555555")
                .await
                .unwrap(),
            "another app's memory"
        );
        assert!(!mem0.delete("org", "o", &ours).await.unwrap(), "right memory, wrong scope");
        assert!(!mem0.delete("repo", "o/r", "../../v1/memories").await.unwrap(), "not an id");
        assert_eq!(mock.memories.lock().unwrap().len(), 2, "nothing was deleted yet");

        assert!(mem0.delete("repo", "o/r", &ours).await.unwrap());
        assert_eq!(mock.memories.lock().unwrap().len(), 1);
        assert!(!mem0.delete("repo", "o/r", &ours).await.unwrap(), "already gone");
    }

    #[tokio::test]
    async fn ranking_follows_the_query() {
        let (mem0, _mock) = client().await;
        let deploy = mem0
            .add(
                &draft(
                    "repo",
                    "o/r",
                    "Deploys",
                    "deploy with the staging workflow first",
                    &[],
                    Value::Null,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let style = mem0
            .add(&draft("org", "o", "Style", "prefer small commits", &[], Value::Null).unwrap())
            .await
            .unwrap();
        let ranks = mem0
            .rank(
                "how do I deploy to staging",
                &[("global", ""), ("org", "o"), ("repo", "o/r")],
                10,
            )
            .await
            .unwrap();
        assert!(ranks[&deploy] < ranks[&style]);
    }

    #[tokio::test]
    async fn a_wrong_key_says_so() {
        let mock = mock::Mock::default();
        let base = mock::serve(mock).await;
        let mem0 = Mem0::new(&base, "wrong".into()).unwrap();
        let error = format!("{:#}", mem0.check().await.unwrap_err());
        assert!(error.contains("rejected the API key"), "{error}");
        assert!(!error.contains("wrong"), "the key must never appear in an error: {error}");
    }

    #[test]
    fn the_base_url_must_be_http() {
        assert!(Mem0::new("api.mem0.ai", "k".into()).is_err());
        assert!(Mem0::new("https://api.mem0.ai/", "k".into()).is_ok());
    }
}
