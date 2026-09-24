//! Chat: the cockpit's direct conversation with a model — no colony, no microVM, no repository.
//!
//! A conversation lives on the Mothership as two files under `<data>/chats/`: `<id>.json` (title,
//! model, system prompt, token limit, workspace) and `<id>.jsonl` (one message per line). Nothing
//! leaves the host except the request to the model the conversation picked.
//!
//! Which models a chat may use follows the summaries' rule, never looser: a `<provider>/<model>` of a
//! configured model provider, or a plain Claude model through the operator's Anthropic provider or a
//! real Anthropic API key (`sk-ant-api…`). The Claude subscription login (`claude setup-token`) is
//! for Claude Code inside colonies and is never sent from here.
//!
//! `POST /api/chat/{id}/messages` answers with newline-delimited JSON, streamed as the model writes:
//! `{"type":"delta","text":…}` lines, then one `{"type":"done","message":…}` (or `{"type":"error"}`).
//! Closing the request stops the reply; what was written so far is kept, marked `stopped`.
//!
//! A conversation can be forked from any message (`POST /api/chat/{id}/fork`), which is also how
//! "edit and resend" branches; one prompt can go to two models side by side
//! (`POST /api/chat/{id}/compare`), whose replies stay candidates until one is picked
//! (`POST /api/chat/{id}/pick`); `GET /api/chat/{id}/export` renders it as Markdown. A message can
//! carry attachments — a colony, a repository file, a map component, a snippet, an image (Anthropic
//! wire only), today's colonies or the week's merged pull requests — which reach the model as system
//! context and are recorded on the message by label only.

use crate::{
    ApiResult, App, Shared, client_error,
    providers::Wire,
    summaries::{self, Route},
    util::{append_line, short_id, write_atomic},
};
use anyhow::{Context, Result};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{StatusCode, header},
    response::Response,
};
use chrono::Utc;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

/// Default and ceiling for a reply's `max_tokens`.
const DEFAULT_MAX_TOKENS: u64 = 4096;
const MAX_TOKENS_CEILING: u64 = 32_000;
/// How much history goes to the model: the most recent messages, capped by count and by bytes.
const HISTORY_MESSAGES: usize = 60;
const HISTORY_BYTES: usize = 200 * 1024;
/// A single message the operator sends, capped.
const MESSAGE_LIMIT: usize = 100 * 1024;
/// The fallback pseudo-org chat spend is filed under when a conversation has no workspace.
pub const CHAT_ORG: &str = "chat";

// ---------------------------------------------------------------------------
// Model choice
// ---------------------------------------------------------------------------

/// Why a Claude model cannot be used from chat, shown in the picker.
pub const NO_CLAUDE_KEY: &str = "a Claude model needs an Anthropic API key (sk-ant-api…) or an Anthropic model provider; \
     the Claude subscription login is only used by colonies";

/// How a chat reaches `model`, or why it cannot. Same rule as [`summaries::choose`] for an explicit
/// model, never the subscription login. Pure, for the tests.
pub fn resolve(model: &str, provider_ids: &[&str], has_anthropic_provider: bool, has_api_key: bool) -> Result<Route, String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("pick a model".into());
    }
    if let Some((id, rest)) = model.split_once('/') {
        if rest.is_empty() || !provider_ids.contains(&id) {
            return Err(format!("no model provider called {id:?} is configured"));
        }
        return Ok(Route::Provider(model.to_string()));
    }
    if has_anthropic_provider {
        Ok(Route::Provider(model.to_string()))
    } else if has_api_key {
        Ok(Route::ApiKey(model.to_string()))
    } else {
        Err(NO_CLAUDE_KEY.into())
    }
}

/// What this install can reach, for [`resolve`].
struct Reach {
    ids: Vec<String>,
    anthropic_provider: bool,
    api_key: Option<String>,
}

fn reach(app: &App) -> Reach {
    let providers = app.providers();
    Reach {
        ids: providers.iter().map(|p| p.id.clone()).collect(),
        anthropic_provider: providers.iter().any(|p| {
            crate::providers::split_url(&p.base_url)
                .is_some_and(|(_, host, _, _)| host.eq_ignore_ascii_case(crate::CLAUDE_API_HOST))
        }),
        api_key: app
            .claude_cred()
            .and_then(|c| summaries::api_key_of(&c.value).map(str::to_string)),
    }
}

fn route_for(app: &App, model: &str) -> Result<(Route, Reach), String> {
    let reach = reach(app);
    let ids: Vec<&str> = reach.ids.iter().map(String::as_str).collect();
    let route = resolve(model, &ids, reach.anthropic_provider, reach.api_key.is_some())?;
    Ok((route, reach))
}

fn route_model(route: &Route) -> &str {
    match route {
        Route::Provider(m) | Route::ApiKey(m) => m,
    }
}

/// `GET /api/chat/models`: the default model (the summaries' cheap one) and whether plain Claude
/// models can be used, with the reason when not.
pub async fn models(State(app): State<Shared>) -> Json<Value> {
    let default = summaries::cheap_route(&app).await.map(|r| route_model(&r).to_string());
    let reach = reach(&app);
    let claude = reach.anthropic_provider || reach.api_key.is_some();
    Json(json!({
        "default": default,
        "claude": {"available": claude, "reason": if claude { Value::Null } else { json!(NO_CLAUDE_KEY) }},
        // Per provider: its wire (images go only over `anthropic`), whether a key is stored and its
        // prices per million tokens, for the model picker. Never the key itself.
        "providers": app
            .providers()
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "models": p.models,
                    "preset": p.preset,
                    "wire": p.wire,
                    "has_key": p.auth == "none" || app.provider_key(&p.id).is_some(),
                    "pricing": p.pricing.map(|r| json!({"input_per_mtok": r.input_per_mtok, "output_per_mtok": r.output_per_mtok})),
                })
            })
            .collect::<Vec<_>>(),
    }))
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ChatMeta {
    pub id: String,
    pub title: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub max_tokens: u64,
    /// The workspace (org) its spend is filed under; `None` files it under [`CHAT_ORG`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Kept at the top of the list.
    pub pinned: bool,
    /// Sampling temperature, 0–1; `None` leaves it to the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// The persona preset the system prompt came from, a label only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// The title is still the automatic one, so the first reply may replace it with a generated one.
    pub auto_title: bool,
    /// Where this conversation was forked from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<ForkRef>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ForkRef {
    pub chat: String,
    pub message: String,
}

/// What an attachment was, kept on the message for display: never the content itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AttachmentNote {
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ChatMessage {
    pub id: String,
    /// `user` or `assistant`.
    pub role: String,
    pub content: String,
    pub ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The reply was cut short: the operator stopped it or the connection closed.
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The message this one answers (a reply's user message), for branching and compare.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Milliseconds to the first streamed token, and to the end of the reply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentNote>,
    /// A compare reply not yet picked; left out of the history the model sees.
    pub candidate: bool,
    /// Which side of a compare it came from (0 or 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<u8>,
}

/// A conversation id is ours: short lowercase hex/alphanumerics, nothing that could be a path.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn dir(app: &App) -> PathBuf {
    app.cfg.data_dir.join("chats")
}

fn meta_path(app: &App, id: &str) -> PathBuf {
    dir(app).join(format!("{id}.json"))
}

fn messages_path(app: &App, id: &str) -> PathBuf {
    dir(app).join(format!("{id}.jsonl"))
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Every message line that parses; a torn last line (a crash mid-append) is skipped.
pub fn parse_messages(text: &str) -> Vec<ChatMessage> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

async fn read_meta(app: &App, id: &str) -> Option<ChatMeta> {
    let bytes = tokio::fs::read(meta_path(app, id)).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn write_meta(app: &App, meta: &ChatMeta) -> Result<()> {
    tokio::fs::create_dir_all(dir(app)).await?;
    write_atomic(&meta_path(app, &meta.id), &serde_json::to_vec_pretty(meta)?).await
}

async fn read_messages(app: &App, id: &str) -> Vec<ChatMessage> {
    tokio::fs::read_to_string(messages_path(app, id))
        .await
        .map(|t| parse_messages(&t))
        .unwrap_or_default()
}

async fn append_message(app: &App, id: &str, message: &ChatMessage) -> Result<()> {
    tokio::fs::create_dir_all(dir(app)).await?;
    append_line(&messages_path(app, id), &serde_json::to_string(message)?).await
}

async fn rewrite_messages(app: &App, id: &str, messages: &[ChatMessage]) -> Result<()> {
    let mut text = String::new();
    for m in messages {
        text.push_str(&serde_json::to_string(m)?);
        text.push('\n');
    }
    write_atomic(&messages_path(app, id), text.as_bytes()).await
}

fn load_or_404(meta: Option<ChatMeta>) -> Result<ChatMeta, crate::AppError> {
    meta.ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such conversation"))
}

fn check_id(id: &str) -> Result<(), crate::AppError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(client_error(StatusCode::BAD_REQUEST, "invalid conversation id"))
    }
}

/// `GET /api/chat`: every conversation, most recently updated first.
pub async fn list(State(app): State<Shared>) -> Json<Value> {
    let mut metas = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(dir(&app)).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(id) = name.strip_suffix(".json")
                && valid_id(id)
                && let Some(meta) = read_meta(&app, id).await
            {
                metas.push(meta);
            }
        }
    }
    metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Json(json!({"chats": metas}))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NewChat {
    pub title: String,
    pub model: String,
    pub system: Option<String>,
    pub max_tokens: Option<u64>,
    pub workspace: Option<String>,
    pub temperature: Option<f64>,
    pub persona: Option<String>,
}

fn clamp_temperature(t: f64) -> f64 {
    if t.is_finite() { t.clamp(0.0, 1.0) } else { 1.0 }
}

fn clamp_tokens(value: Option<u64>) -> u64 {
    value.unwrap_or(DEFAULT_MAX_TOKENS).clamp(1, MAX_TOKENS_CEILING)
}

/// `POST /api/chat`: a new, empty conversation. With no model it takes the cheap default.
pub async fn create(State(app): State<Shared>, Json(req): Json<NewChat>) -> ApiResult<ChatMeta> {
    let model = if req.model.trim().is_empty() {
        summaries::cheap_route(&app)
            .await
            .map(|r| route_model(&r).to_string())
            .unwrap_or_default()
    } else {
        req.model.trim().to_string()
    };
    let at = now();
    let meta = ChatMeta {
        id: short_id(),
        title: req.title.trim().chars().take(120).collect(),
        model,
        system: req.system.filter(|s| !s.trim().is_empty()),
        max_tokens: clamp_tokens(req.max_tokens),
        workspace: req.workspace.filter(|w| !w.trim().is_empty()),
        created_at: at.clone(),
        updated_at: at,
        pinned: false,
        temperature: req.temperature.map(clamp_temperature),
        persona: req.persona.filter(|p| !p.trim().is_empty()),
        auto_title: req.title.trim().is_empty(),
        forked_from: None,
    };
    write_meta(&app, &meta).await?;
    Ok(Json(meta))
}

/// `GET /api/chat/{id}`: the conversation and its messages.
pub async fn get(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    check_id(&id)?;
    let meta = load_or_404(read_meta(&app, &id).await)?;
    let messages = read_messages(&app, &id).await;
    Ok(Json(json!({"chat": meta, "messages": messages})))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PatchChat {
    pub title: Option<String>,
    pub model: Option<String>,
    pub system: Option<String>,
    pub max_tokens: Option<u64>,
    pub workspace: Option<String>,
    pub pinned: Option<bool>,
    /// A number sets it; `null` in JSON cannot be told apart from absent, so `-1` clears it.
    pub temperature: Option<f64>,
    pub persona: Option<String>,
}

/// `PATCH /api/chat/{id}`: rename, pin, or change the model, system prompt, persona, temperature,
/// token limit or workspace.
pub async fn patch(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<PatchChat>) -> ApiResult<ChatMeta> {
    check_id(&id)?;
    let mut meta = load_or_404(read_meta(&app, &id).await)?;
    if let Some(title) = req.title {
        meta.title = title.trim().chars().take(120).collect();
        meta.auto_title = false;
    }
    if let Some(pinned) = req.pinned {
        meta.pinned = pinned;
    }
    if let Some(t) = req.temperature {
        meta.temperature = if t < 0.0 { None } else { Some(clamp_temperature(t)) };
    }
    if let Some(persona) = req.persona {
        meta.persona = Some(persona).filter(|p| !p.trim().is_empty());
    }
    if let Some(model) = req.model {
        meta.model = model.trim().to_string();
    }
    if let Some(system) = req.system {
        meta.system = Some(system).filter(|s| !s.trim().is_empty());
    }
    if req.max_tokens.is_some() {
        meta.max_tokens = clamp_tokens(req.max_tokens);
    }
    if let Some(workspace) = req.workspace {
        meta.workspace = Some(workspace).filter(|w| !w.trim().is_empty());
    }
    meta.updated_at = now();
    write_meta(&app, &meta).await?;
    Ok(Json(meta))
}

/// `DELETE /api/chat/{id}`: removes both files.
pub async fn delete(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    check_id(&id)?;
    load_or_404(read_meta(&app, &id).await)?;
    let _ = tokio::fs::remove_file(messages_path(&app, &id)).await;
    tokio::fs::remove_file(meta_path(&app, &id)).await?;
    Ok(Json(json!({"deleted": id})))
}

// ---------------------------------------------------------------------------
// Sending: the model's reply, streamed
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Send {
    pub content: String,
    /// Drops the last assistant reply and asks again for the same user message.
    pub regenerate: bool,
    /// Asks this model instead of the conversation's (regenerate with another model); the
    /// conversation's own model is unchanged.
    pub model: Option<String>,
    /// Older clients' context: a colony and/or a repository file. New clients send `attachments`.
    pub context: Option<ChatContext>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChatContext {
    pub colony: Option<String>,
    /// A repository file, read from the Mothership's clone (the Code page's blob reader).
    pub file: Option<FileContext>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileContext {
    pub repo: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

/// Something attached to a message. It reaches the model as system context (an image, as an image
/// block on the message); the message keeps only its [`AttachmentNote`].
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attachment {
    /// A colony's summary and recent activity.
    Colony { id: String },
    /// A repository file from the Mothership's clone.
    File {
        repo: String,
        path: String,
        #[serde(rename = "ref", default)]
        reference: Option<String>,
    },
    /// A repository's whole stored architecture map, every component described.
    Map { repo: String },
    /// One component of a repository's stored architecture map, with its files and connections.
    MapComponent { repo: String, component: String },
    /// Text pasted by the operator.
    Snippet {
        #[serde(default)]
        label: String,
        text: String,
    },
    /// An image, base64, for models on the Anthropic wire.
    Image {
        media_type: String,
        data: String,
        #[serde(default)]
        name: String,
    },
    /// What the colonies did in the last 24 hours, optionally in one workspace.
    ColoniesToday {
        #[serde(default)]
        org: Option<String>,
    },
    /// Pull requests colonies merged in the last `days` (default 7, at most 31).
    MergedPrs {
        #[serde(default)]
        org: Option<String>,
        #[serde(default)]
        days: Option<u32>,
    },
}

/// How much of an attached file or snippet goes to the model.
const FILE_CONTEXT_LIMIT: usize = 60 * 1024;
/// At most this many attachments on one message, and this much context in all.
const ATTACHMENT_COUNT: usize = 8;
const CONTEXT_LIMIT: usize = 240 * 1024;
/// An image, as base64, at most 5 MB of it.
const IMAGE_LIMIT: usize = 5 * 1024 * 1024;
/// A send or compare body: the message, attachments and room for images, as JSON.
pub const BODY_LIMIT: usize = 12 * 1024 * 1024;
const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Checks an attachment before anything is read for it: sizes, the image type and its base64, repo
/// and path shapes. Pure, for the tests.
pub fn check_attachment(a: &Attachment) -> Result<(), String> {
    match a {
        Attachment::Colony { id } if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Err("invalid colony id".into())
        }
        Attachment::File { repo, path, .. } | Attachment::MapComponent { repo, component: path } => {
            if !crate::util::valid_repo(repo) {
                return Err(format!("invalid repository {repo:?}"));
            }
            if path.trim().is_empty() || path.len() > 1024 {
                return Err("an empty or over-long path".into());
            }
            if matches!(a, Attachment::File { .. })
                && (path.starts_with('/') || path.split('/').any(|seg| seg == ".." || seg == "." || seg.is_empty()))
            {
                return Err(format!("{path:?} is not a repository-relative path"));
            }
            Ok(())
        }
        Attachment::Map { repo } if !crate::util::valid_repo(repo) => Err(format!("invalid repository {repo:?}")),
        Attachment::Snippet { text, .. } if text.len() > FILE_CONTEXT_LIMIT => Err("a snippet is over 60 KB".into()),
        Attachment::Snippet { text, .. } if text.trim().is_empty() => Err("an empty snippet".into()),
        Attachment::Image { media_type, data, .. } => {
            if !IMAGE_TYPES.contains(&media_type.as_str()) {
                return Err(format!("{media_type} images are not supported (png, jpeg, gif, webp)"));
            }
            if data.len() > IMAGE_LIMIT {
                return Err("an image is over 5 MB".into());
            }
            if data.is_empty()
                || !data
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
            {
                return Err("the image is not base64".into());
            }
            Ok(())
        }
        Attachment::MergedPrs { days: Some(d), .. } if *d == 0 || *d > 31 => Err("days must be 1–31".into()),
        _ => Ok(()),
    }
}

/// What the model is told and what the message records, built from its attachments.
#[derive(Default)]
struct Built {
    system: String,
    notes: Vec<AttachmentNote>,
    images: Vec<Value>,
}

fn in_org(repo: &str, org: Option<&str>) -> bool {
    org.is_none_or(|o| repo.split('/').next().is_some_and(|owner| owner.eq_ignore_ascii_case(o)))
}

/// A line per colony that moved in the last 24 hours. Pure over the list, for the tests.
pub fn colonies_digest(sessions: &[crate::sessions::Session], org: Option<&str>, now: chrono::DateTime<Utc>) -> String {
    let since = now - chrono::Duration::hours(24);
    let mut rows: Vec<&crate::sessions::Session> = sessions
        .iter()
        .filter(|s| s.updated_at >= since && in_org(&s.repo, org))
        .collect();
    rows.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
    rows.iter()
        .take(40)
        .map(|s| {
            format!(
                "- [{}] {}{}: {}{}",
                s.status.as_str(),
                s.repo,
                s.issue.map(|n| format!(" #{n}")).unwrap_or_default(),
                s.summary.clone().unwrap_or_else(|| s.issue_title.clone()),
                s.pr_url.as_deref().map(|u| format!(" ({u})")).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A line per pull request merged in the last `days`. Pure over the list, for the tests.
pub fn merged_digest(sessions: &[crate::sessions::Session], org: Option<&str>, days: u32, now: chrono::DateTime<Utc>) -> String {
    let since = now - chrono::Duration::days(i64::from(days));
    let mut rows: Vec<&crate::sessions::Session> = sessions
        .iter()
        .filter(|s| s.merged_at.is_some_and(|m| m >= since) && in_org(&s.repo, org))
        .collect();
    rows.sort_by_key(|a| std::cmp::Reverse(a.merged_at));
    rows.iter()
        .take(60)
        .map(|s| {
            format!(
                "- {}: {}{} (merged {})",
                s.repo,
                s.summary.clone().unwrap_or_else(|| s.issue_title.clone()),
                s.pr_url.as_deref().map(|u| format!(" — {u}")).unwrap_or_default(),
                s.merged_at.map(|m| m.format("%Y-%m-%d").to_string()).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One map component described for the model: what it is, its files, and what it connects to.
/// Pure over the stored map, for the tests.
pub fn describe_component(stored: &Value, component: &str) -> Option<(String, String)> {
    let map = &stored["map"];
    let comps = map["components"].as_array()?;
    let c = comps.iter().find(|c| {
        c["id"].as_str() == Some(component) || c["label"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(component))
    })?;
    let id = c["id"].as_str().unwrap_or_default();
    let label = c["label"].as_str().unwrap_or(id).to_string();
    let files: Vec<&str> = c["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["path"].as_str())
        .collect();
    let links: Vec<String> = map["connections"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|k| k["from"].as_str() == Some(id) || k["to"].as_str() == Some(id))
        .map(|k| {
            format!(
                "{} → {}{}",
                k["from"].as_str().unwrap_or("?"),
                k["to"].as_str().unwrap_or("?"),
                k["label"].as_str().map(|l| format!(" ({l})")).unwrap_or_default()
            )
        })
        .collect();
    let text = format!(
        "{label} ({}{}): files {}; connections {}",
        c["type"].as_str().unwrap_or("component"),
        c["sublabel"].as_str().map(|s| format!(", {s}")).unwrap_or_default(),
        if files.is_empty() {
            "none listed".to_string()
        } else {
            files.join(", ")
        },
        if links.is_empty() {
            "none".to_string()
        } else {
            links.join("; ")
        }
    );
    Some((label, text))
}

/// Every component of a stored map, one line each. Pure, for the tests.
pub fn describe_map(stored: &Value) -> String {
    let ids: Vec<&str> = stored["map"]["components"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["id"].as_str())
        .collect();
    let mut out = String::new();
    for id in ids {
        if let Some((_, text)) = describe_component(stored, id) {
            out.push_str(&format!("\n- {text}"));
        }
        if out.len() > FILE_CONTEXT_LIMIT {
            out.push_str("\n- (more components left out)");
            break;
        }
    }
    out
}

async fn build_attachments(app: &Shared, attachments: &[Attachment], images_allowed: bool) -> Result<Built, crate::AppError> {
    let bad = |m: String| client_error(StatusCode::BAD_REQUEST, &m);
    if attachments.len() > ATTACHMENT_COUNT {
        return Err(bad(format!("at most {ATTACHMENT_COUNT} attachments on one message")));
    }
    let mut out = Built::default();
    for a in attachments {
        check_attachment(a).map_err(bad)?;
        match a {
            Attachment::Colony { id } => {
                let s = app.session(id).await.ok_or_else(|| bad(format!("no colony {id}")))?;
                let recent = crate::autonomy::event_context(&app.session_dir(&s.id).join("events.jsonl")).await;
                let summary = s.summary.clone().unwrap_or_else(|| s.issue_title.clone());
                out.system.push_str(&format!(
                    "\n\nContext — colony {} on {} ({}{}): {}\nRecent activity:\n{}",
                    s.id,
                    s.repo,
                    s.status.as_str(),
                    s.error.as_deref().map(|e| format!(", error: {e}")).unwrap_or_default(),
                    summary,
                    recent.join("\n")
                ));
                out.notes.push(AttachmentNote {
                    kind: "colony".into(),
                    label: crate::util::truncate(&summary, 80).to_string(),
                });
            }
            Attachment::File { repo, path, reference } => {
                let (owner, name) = repo.split_once('/').ok_or_else(|| bad("invalid repository".into()))?;
                let answer = crate::code::blob(
                    State(app.clone()),
                    Path((owner.to_string(), name.to_string())),
                    axum::extract::Query(crate::code::PathQuery {
                        path: path.clone(),
                        reference: reference.clone(),
                    }),
                )
                .await?;
                let text = answer.0["text"].as_str().unwrap_or_default();
                if text.is_empty() {
                    return Err(bad(format!("{path} is binary, too large or empty")));
                }
                let cut: String = text.chars().take(FILE_CONTEXT_LIMIT).collect();
                let at = answer.0["ref"].as_str().unwrap_or("HEAD");
                out.system.push_str(&format!(
                    "\n\nContext — file {repo}/{path} at {at}{}:\n```\n{cut}\n```",
                    if cut.len() < text.len() { " (truncated)" } else { "" }
                ));
                out.notes.push(AttachmentNote {
                    kind: "file".into(),
                    label: format!("{repo}/{path}"),
                });
            }
            Attachment::Map { repo } => {
                let stored = crate::maps::read_stored(app, repo).ok_or_else(|| bad(format!("{repo} has no map yet")))?;
                out.system.push_str(&format!(
                    "\n\nContext — {repo}'s architecture map (revision {}), components:{}",
                    stored["revision"].as_str().unwrap_or("unknown"),
                    describe_map(&stored)
                ));
                out.notes.push(AttachmentNote {
                    kind: "map".into(),
                    label: format!("{repo} map"),
                });
            }
            Attachment::MapComponent { repo, component } => {
                let stored = crate::maps::read_stored(app, repo).ok_or_else(|| bad(format!("{repo} has no map yet")))?;
                let (label, text) = describe_component(&stored, component)
                    .ok_or_else(|| bad(format!("no component {component:?} in {repo}'s map")))?;
                out.system.push_str(&format!("\n\nContext — {repo} architecture, {text}"));
                out.notes.push(AttachmentNote {
                    kind: "map".into(),
                    label: format!("{repo} · {label}"),
                });
            }
            Attachment::Snippet { label, text } => {
                let label = if label.trim().is_empty() { "snippet" } else { label.trim() };
                out.system.push_str(&format!("\n\nContext — {label}:\n```\n{text}\n```"));
                out.notes.push(AttachmentNote {
                    kind: "snippet".into(),
                    label: label.chars().take(60).collect(),
                });
            }
            Attachment::Image { media_type, data, name } => {
                if !images_allowed {
                    return Err(bad(
                        "this model is reached over an OpenAI-style provider, which images are not sent to; pick a Claude or Anthropic-wire model"
                            .into(),
                    ));
                }
                out.images
                    .push(json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}}));
                out.notes.push(AttachmentNote {
                    kind: "image".into(),
                    label: if name.trim().is_empty() {
                        media_type.clone()
                    } else {
                        name.chars().take(60).collect()
                    },
                });
            }
            Attachment::ColoniesToday { org } => {
                let sessions = app.sessions.read().await.clone();
                let digest = colonies_digest(&sessions, org.as_deref(), Utc::now());
                out.system.push_str(&format!(
                    "\n\nContext — colonies that moved in the last 24 hours{}:\n{}",
                    org.as_deref().map(|o| format!(" in {o}")).unwrap_or_default(),
                    if digest.is_empty() { "(none)".to_string() } else { digest }
                ));
                out.notes.push(AttachmentNote {
                    kind: "colonies".into(),
                    label: format!(
                        "today's colonies{}",
                        org.as_deref().map(|o| format!(" · {o}")).unwrap_or_default()
                    ),
                });
            }
            Attachment::MergedPrs { org, days } => {
                let days = days.unwrap_or(7);
                let sessions = app.sessions.read().await.clone();
                let digest = merged_digest(&sessions, org.as_deref(), days, Utc::now());
                out.system.push_str(&format!(
                    "\n\nContext — pull requests colonies merged in the last {days} days{}:\n{}",
                    org.as_deref().map(|o| format!(" in {o}")).unwrap_or_default(),
                    if digest.is_empty() { "(none)".to_string() } else { digest }
                ));
                out.notes.push(AttachmentNote {
                    kind: "merged".into(),
                    label: format!(
                        "merged PRs · {days}d{}",
                        org.as_deref().map(|o| format!(" · {o}")).unwrap_or_default()
                    ),
                });
            }
        }
        if out.system.len() > CONTEXT_LIMIT {
            return Err(client_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "the attachments are over 240 KB together",
            ));
        }
    }
    Ok(out)
}

/// Whether the route sends Anthropic-wire requests (so image blocks can go through).
fn anthropic_wire(app: &App, route: &Route) -> bool {
    match route {
        Route::ApiKey(_) => true,
        Route::Provider(m) => {
            let providers = app.providers();
            crate::autonomy::route(m, &providers).is_ok_and(|(p, _)| p.wire == Wire::Anthropic)
        }
    }
}

/// The Anthropic-shaped message list the model sees: the most recent history (errors and empty
/// replies left out), trimmed to [`HISTORY_MESSAGES`] and [`HISTORY_BYTES`], starting on a user turn
/// as the API requires. Pure, for the tests.
pub fn history_for_model(messages: &[ChatMessage]) -> Vec<Value> {
    let usable: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| {
            (m.role == "user" || m.role == "assistant") && m.error.is_none() && !m.candidate && !m.content.trim().is_empty()
        })
        .collect();
    let mut kept = Vec::new();
    let mut bytes = 0;
    for m in usable.iter().rev() {
        if kept.len() >= HISTORY_MESSAGES || bytes + m.content.len() > HISTORY_BYTES {
            break;
        }
        bytes += m.content.len();
        kept.push(*m);
    }
    kept.reverse();
    while kept.first().is_some_and(|m| m.role != "user") {
        kept.remove(0);
    }
    // Two turns of the same role in a row (a stopped reply followed by a new question) merge, so
    // the list alternates the way the Messages API wants.
    let mut out: Vec<Value> = Vec::new();
    for m in kept {
        if let Some(last) = out.last_mut()
            && last["role"] == m.role
        {
            let joined = format!("{}\n\n{}", last["content"].as_str().unwrap_or_default(), m.content);
            last["content"] = json!(joined);
            continue;
        }
        out.push(json!({"role": m.role, "content": m.content}));
    }
    out
}

/// A server-sent-events decoder over arbitrary byte chunks: fed pieces of the stream, it hands back
/// each complete event's `data` payload.
#[derive(Default)]
pub struct SseDecoder {
    buf: String,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
        }
        out
    }
}

/// What one Anthropic stream event adds to the reply.
#[derive(Debug, Default, PartialEq)]
pub struct StreamStep {
    pub text: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub error: Option<String>,
}

/// Reads one Anthropic Messages streaming event. Pure, for the tests.
pub fn stream_step(data: &str) -> StreamStep {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return StreamStep::default();
    };
    let tokens = |u: &Value| {
        ["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]
            .iter()
            .filter_map(|k| u[*k].as_u64())
            .sum::<u64>()
    };
    match v["type"].as_str() {
        Some("content_block_delta") if v["delta"]["type"] == "text_delta" => StreamStep {
            text: v["delta"]["text"].as_str().map(str::to_string),
            ..StreamStep::default()
        },
        Some("message_start") => StreamStep {
            input_tokens: Some(tokens(&v["message"]["usage"])),
            output_tokens: v["message"]["usage"]["output_tokens"].as_u64(),
            ..StreamStep::default()
        },
        Some("message_delta") => StreamStep {
            output_tokens: v["usage"]["output_tokens"].as_u64(),
            ..StreamStep::default()
        },
        Some("error") => StreamStep {
            error: Some(
                v["error"]["message"]
                    .as_str()
                    .unwrap_or("the model reported an error")
                    .to_string(),
            ),
            ..StreamStep::default()
        },
        _ => StreamStep::default(),
    }
}

/// The request to send and how its reply comes back.
struct Prepared {
    request: reqwest::RequestBuilder,
    /// Anthropic-wire streaming (`true`), or one whole translated reply (`openai` wire).
    streaming: bool,
    openai: Option<crate::openai::RequestInfo>,
    pricing: Option<crate::providers::Pricing>,
}

fn prepare(app: &App, route: &Route, reach: &Reach, mut body: Value) -> Result<Prepared> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(600))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()?;
    match route {
        Route::ApiKey(model) => {
            let key = reach.api_key.clone().context("no Anthropic API key")?;
            body["model"] = json!(crate::providers::api_model(model));
            body["stream"] = json!(true);
            Ok(Prepared {
                request: client
                    .post("https://api.anthropic.com/v1/messages")
                    .header("anthropic-version", "2023-06-01")
                    .header("x-api-key", key)
                    .header("content-type", "application/json")
                    .body(serde_json::to_vec(&body)?),
                streaming: true,
                openai: None,
                pricing: None,
            })
        }
        Route::Provider(model) => {
            let providers = app.providers();
            let (provider, upstream) = crate::autonomy::route(model, &providers)?;
            body["model"] = json!(upstream);
            let streaming = provider.wire == Wire::Anthropic;
            body["stream"] = json!(streaming);
            let out = crate::autonomy::outbound_request(provider, &body)?;
            let mut request = client.post(out.url).body(out.body);
            for (name, value) in out.headers {
                request = request.header(name, value);
            }
            if let Some((name, value)) = crate::gateway::credential_header(app, provider) {
                request = request.header(name, value);
            }
            Ok(Prepared {
                request,
                streaming,
                openai: out.openai,
                pricing: provider.pricing,
            })
        }
    }
}

fn line(value: Value) -> Bytes {
    let mut s = value.to_string();
    s.push('\n');
    Bytes::from(s)
}

fn ndjson(rx: tokio::sync::mpsc::Receiver<Bytes>) -> Response {
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<_, std::io::Error>(b), rx))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .expect("a fixed response")
}

/// Stores a user message with its attachment notes, answering the conversation's last message.
async fn push_user(
    app: &App,
    meta: &mut ChatMeta,
    messages: &mut Vec<ChatMessage>,
    content: &str,
    notes: Vec<AttachmentNote>,
) -> Result<ChatMessage, crate::AppError> {
    let content = content.trim();
    if content.is_empty() {
        return Err(client_error(StatusCode::BAD_REQUEST, "an empty message"));
    }
    if content.len() > MESSAGE_LIMIT {
        return Err(client_error(StatusCode::PAYLOAD_TOO_LARGE, "the message is over 100 KB"));
    }
    let user = ChatMessage {
        id: short_id(),
        role: "user".into(),
        content: content.to_string(),
        ts: now(),
        parent_id: messages.iter().rev().find(|m| !m.candidate).map(|m| m.id.clone()),
        attachments: notes,
        ..ChatMessage::default()
    };
    append_message(app, &meta.id, &user).await?;
    messages.push(user.clone());
    if meta.title.is_empty() {
        meta.title = content.lines().next().unwrap_or_default().chars().take(60).collect();
        meta.auto_title = true;
    }
    Ok(user)
}

/// The request body: history, system prompt with the attachments' context, temperature, and any
/// images on the newest user turn.
fn request_body(meta: &ChatMeta, messages: &[ChatMessage], extra_system: &str, images: &[Value]) -> Value {
    let mut history = history_for_model(messages);
    if !images.is_empty()
        && let Some(last) = history.last_mut()
        && last["role"] == "user"
    {
        let text = last["content"].as_str().unwrap_or_default().to_string();
        let mut blocks: Vec<Value> = images.to_vec();
        blocks.push(json!({"type": "text", "text": text}));
        last["content"] = Value::Array(blocks);
    }
    let mut body = json!({"max_tokens": meta.max_tokens, "messages": history});
    let system = format!("{}{}", meta.system.clone().unwrap_or_default(), extra_system);
    if !system.trim().is_empty() {
        body["system"] = json!(system.trim());
    }
    if let Some(t) = meta.temperature {
        body["temperature"] = json!(t);
    }
    body
}

/// `POST /api/chat/{id}/messages`: stores the operator's message, asks the conversation's model (or
/// `model`, for a regenerate with another one) and streams its reply as newline-delimited JSON; the
/// reply is stored when it ends or is stopped.
pub async fn send(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<Send>) -> Result<Response, crate::AppError> {
    check_id(&id)?;
    let mut meta = load_or_404(read_meta(&app, &id).await)?;
    let model = req
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| meta.model.clone());
    let (route, reach) = route_for(&app, &model).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    let mut messages = read_messages(&app, &id).await;

    // The older `context` shape becomes attachments, so both kinds of client work.
    let mut attachments = req.attachments.clone();
    if let Some(ctx) = &req.context {
        if let Some(colony) = &ctx.colony {
            attachments.push(Attachment::Colony { id: colony.clone() });
        }
        if let Some(f) = &ctx.file {
            attachments.push(Attachment::File {
                repo: f.repo.clone(),
                path: f.path.clone(),
                reference: f.reference.clone(),
            });
        }
    }
    let built = build_attachments(&app, &attachments, anthropic_wire(&app, &route)).await?;

    let parent = if req.regenerate {
        while messages.last().is_some_and(|m| m.role == "assistant") {
            messages.pop();
        }
        let Some(last) = messages.last().filter(|m| m.role == "user") else {
            return Err(client_error(StatusCode::BAD_REQUEST, "nothing to regenerate"));
        };
        let parent = last.id.clone();
        rewrite_messages(&app, &id, &messages).await?;
        parent
    } else {
        push_user(&app, &mut meta, &mut messages, &req.content, built.notes.clone())
            .await?
            .id
    };

    let body = request_body(&meta, &messages, &built.system, &built.images);
    let prepared = prepare(&app, &route, &reach, body).map_err(|e| client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")))?;
    meta.updated_at = now();
    write_meta(&app, &meta).await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(64);
    let opts = ReplyOpts {
        parent_id: Some(parent),
        lane: None,
        candidate: false,
    };
    tokio::spawn(run_reply(
        app.clone(),
        meta,
        route_model(&route).to_string(),
        prepared,
        tx,
        opts,
    ));
    Ok(ndjson(rx))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Compare {
    pub content: String,
    pub models: Vec<String>,
    pub attachments: Vec<Attachment>,
}

/// `POST /api/chat/{id}/compare`: one prompt to two models at once. Both replies stream on one
/// response, each line tagged with its `lane` (0 or 1), and are stored as candidates the model's
/// history leaves out until one is picked.
pub async fn compare(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<Compare>,
) -> Result<Response, crate::AppError> {
    check_id(&id)?;
    if req.models.len() != 2 || req.models[0].trim() == req.models[1].trim() {
        return Err(client_error(StatusCode::BAD_REQUEST, "compare needs two different models"));
    }
    let mut meta = load_or_404(read_meta(&app, &id).await)?;
    let mut routes = Vec::new();
    for m in &req.models {
        routes.push(route_for(&app, m).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?);
    }
    let images_allowed = routes.iter().all(|(r, _)| anthropic_wire(&app, r));
    let built = build_attachments(&app, &req.attachments, images_allowed).await?;
    let mut messages = read_messages(&app, &id).await;
    let user = push_user(&app, &mut meta, &mut messages, &req.content, built.notes.clone()).await?;
    let body = request_body(&meta, &messages, &built.system, &built.images);
    let mut prepared = Vec::new();
    for (route, reach) in &routes {
        prepared.push(
            prepare(&app, route, reach, body.clone()).map_err(|e| client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")))?,
        );
    }
    meta.updated_at = now();
    write_meta(&app, &meta).await?;
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(128);
    for (lane, ((route, _), p)) in routes.iter().zip(prepared).enumerate() {
        let opts = ReplyOpts {
            parent_id: Some(user.id.clone()),
            lane: Some(lane as u8),
            candidate: true,
        };
        tokio::spawn(run_reply(
            app.clone(),
            meta.clone(),
            route_model(route).to_string(),
            p,
            tx.clone(),
            opts,
        ));
    }
    drop(tx);
    Ok(ndjson(rx))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Pick {
    pub message_id: String,
}

/// `POST /api/chat/{id}/pick`: keeps one compare candidate as the reply and drops the other.
pub async fn pick(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<Pick>) -> ApiResult<Value> {
    check_id(&id)?;
    load_or_404(read_meta(&app, &id).await)?;
    let messages = read_messages(&app, &id).await;
    let messages = pick_candidate(messages, &req.message_id).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    rewrite_messages(&app, &id, &messages).await?;
    Ok(Json(json!({"messages": messages})))
}

/// The messages with `chosen` kept as an ordinary reply and its sibling candidates removed. Pure,
/// for the tests.
pub fn pick_candidate(messages: Vec<ChatMessage>, chosen: &str) -> Result<Vec<ChatMessage>, String> {
    let Some(pick) = messages.iter().find(|m| m.id == chosen && m.candidate) else {
        return Err("no such compare reply".into());
    };
    let parent = pick.parent_id.clone();
    Ok(messages
        .into_iter()
        .filter(|m| !(m.candidate && m.parent_id == parent && m.id != chosen))
        .map(|mut m| {
            if m.id == chosen {
                m.candidate = false;
                m.lane = None;
            }
            m
        })
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct Fork {
    pub message_id: String,
    /// Keep the message itself (fork *at* it) or stop just before it (edit and resend).
    #[serde(default = "yes")]
    pub include: bool,
}

fn yes() -> bool {
    true
}

/// The messages a fork at `message_id` keeps, compare candidates left out. Pure, for the tests.
pub fn fork_messages(messages: &[ChatMessage], message_id: &str, include: bool) -> Option<Vec<ChatMessage>> {
    let at = messages.iter().position(|m| m.id == message_id)?;
    let end = if include { at + 1 } else { at };
    Some(messages[..end].iter().filter(|m| !m.candidate).cloned().collect())
}

/// `POST /api/chat/{id}/fork`: a new conversation with the messages up to one of this one's.
pub async fn fork(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<Fork>) -> ApiResult<ChatMeta> {
    check_id(&id)?;
    let source = load_or_404(read_meta(&app, &id).await)?;
    let messages = read_messages(&app, &id).await;
    let kept = fork_messages(&messages, &req.message_id, req.include)
        .ok_or_else(|| client_error(StatusCode::BAD_REQUEST, "no such message"))?;
    let at = now();
    let base = if source.title.is_empty() {
        "conversation".to_string()
    } else {
        source.title.clone()
    };
    let meta = ChatMeta {
        id: short_id(),
        title: format!("{} (branch)", base.chars().take(100).collect::<String>()),
        created_at: at.clone(),
        updated_at: at,
        pinned: false,
        auto_title: false,
        forked_from: Some(ForkRef {
            chat: source.id.clone(),
            message: req.message_id.clone(),
        }),
        ..source
    };
    write_meta(&app, &meta).await?;
    rewrite_messages(&app, &meta.id, &kept).await?;
    Ok(Json(meta))
}

/// A conversation as Markdown. Pure, for the tests.
pub fn to_markdown(meta: &ChatMeta, messages: &[ChatMessage]) -> String {
    let mut out = format!(
        "# {}\n\n_Model: {} · started {}_\n",
        if meta.title.is_empty() { "Conversation" } else { &meta.title },
        meta.model,
        meta.created_at
    );
    if let Some(system) = meta.system.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push_str(&format!("\n> **System:** {}\n", system.trim().replace('\n', "\n> ")));
    }
    for m in messages.iter().filter(|m| !m.candidate) {
        let who = if m.role == "user" {
            "You".to_string()
        } else {
            format!("Assistant ({})", m.model.as_deref().unwrap_or("?"))
        };
        out.push_str(&format!("\n## {who}\n\n"));
        if !m.attachments.is_empty() {
            let labels: Vec<String> = m.attachments.iter().map(|a| format!("{}: {}", a.kind, a.label)).collect();
            out.push_str(&format!("_Attached: {}_\n\n", labels.join(", ")));
        }
        out.push_str(m.content.trim());
        out.push('\n');
        if let Some(e) = &m.error {
            out.push_str(&format!("\n_Error: {e}_\n"));
        }
    }
    out
}

/// `GET /api/chat/{id}/export`: the conversation as a Markdown download.
pub async fn export(State(app): State<Shared>, Path(id): Path<String>) -> Result<Response, crate::AppError> {
    check_id(&id)?;
    let meta = load_or_404(read_meta(&app, &id).await)?;
    let messages = read_messages(&app, &id).await;
    let md = to_markdown(&meta, &messages);
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "text/markdown; charset=utf-8")
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"chat-{id}.md\""))
        .body(Body::from(md))
        .expect("a fixed response"))
}

/// A generated title cleaned up: its first line, no quotes, "Title:" or trailing punctuation, at
/// most 60 characters. `None` when nothing is left. Pure, for the tests.
pub fn clean_title(answer: &str) -> Option<String> {
    let line = answer.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut t: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    for prefix in ["Title:", "title:", "TITLE:"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.trim().to_string();
        }
    }
    let quotes: &[char] = &['"', '\'', '“', '”', '‘', '’', '`', '*', '#'];
    t = t.trim_matches(quotes).trim().to_string();
    while t.ends_with(['.', ':', ';', '!']) {
        t.pop();
    }
    let t = t.trim().to_string();
    if t.is_empty() {
        return None;
    }
    Some(if t.chars().count() > 60 {
        format!("{}…", t.chars().take(59).collect::<String>().trim_end())
    } else {
        t
    })
}

/// Asks the cheap summary model (never the subscription login) for a title from the first exchange.
async fn generate_title(app: &Shared, messages: &[ChatMessage]) -> Result<String, String> {
    let exchange: String = messages
        .iter()
        .filter(|m| !m.candidate && m.error.is_none())
        .take(2)
        .map(|m| {
            format!(
                "{}: {}",
                if m.role == "user" { "User" } else { "Assistant" },
                m.content.chars().take(2000).collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if exchange.is_empty() {
        return Err("nothing to title yet".into());
    }
    let prompt =
        format!("Write a title of 3 to 6 words for this conversation. Answer with the title only, no quotes.\n\n{exchange}");
    let (answer, _) = summaries::ask_freeform(app, &prompt).await?;
    clean_title(&answer).ok_or_else(|| "the model gave no title".into())
}

/// `POST /api/chat/{id}/title`: a new title from the cheap model.
pub async fn retitle(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<ChatMeta> {
    check_id(&id)?;
    let mut meta = load_or_404(read_meta(&app, &id).await)?;
    let messages = read_messages(&app, &id).await;
    let title = generate_title(&app, &messages)
        .await
        .map_err(|e| client_error(StatusCode::BAD_GATEWAY, &e))?;
    meta.title = title;
    meta.auto_title = false;
    meta.updated_at = now();
    write_meta(&app, &meta).await?;
    Ok(Json(meta))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NewIssue {
    pub repo: String,
    pub title: String,
    pub body: String,
}

/// An issue title and body checked before anything reaches GitHub. Pure, for the tests.
pub fn check_issue(req: &NewIssue) -> Result<(), String> {
    if !crate::util::valid_repo(&req.repo) {
        return Err(format!("invalid repository {:?}", req.repo));
    }
    let title = req.title.trim();
    if title.is_empty() || title.chars().count() > 256 || title.contains('\n') {
        return Err("a title is one line of 1–256 characters".into());
    }
    if req.body.len() > FILE_CONTEXT_LIMIT {
        return Err("the issue body is over 60 KB".into());
    }
    Ok(())
}

/// `POST /api/chat/{id}/issue`: files `{repo, title, body}` as a GitHub issue with the
/// Mothership's `gh`, and answers `{url}`. The cockpit asks the operator to confirm first.
pub async fn file_issue(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<NewIssue>) -> ApiResult<Value> {
    check_id(&id)?;
    load_or_404(read_meta(&app, &id).await)?;
    check_issue(&req).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    let body_path = dir(&app).join(format!("{id}.issue-{}.md", short_id()));
    tokio::fs::write(&body_path, format!("{}\n", req.body.trim())).await?;
    let mut cmd = app.gh([
        "issue",
        "create",
        "-R",
        req.repo.as_str(),
        "--title",
        req.title.trim(),
        "--body-file",
    ]);
    cmd.arg(&body_path);
    let out = crate::util::exec(&mut cmd).await;
    let _ = tokio::fs::remove_file(&body_path).await;
    let out = out.map_err(|e| client_error(StatusCode::BAD_GATEWAY, &format!("gh could not file the issue: {e:#}")))?;
    let url = out.lines().rev().find(|l| l.starts_with("https://")).unwrap_or(out.trim());
    Ok(Json(json!({"url": crate::util::truncate(url, 500)})))
}

struct ReplyOpts {
    parent_id: Option<String>,
    lane: Option<u8>,
    candidate: bool,
}

async fn run_reply(
    app: Shared,
    meta: ChatMeta,
    model: String,
    prepared: Prepared,
    tx: tokio::sync::mpsc::Sender<Bytes>,
    opts: ReplyOpts,
) {
    let started = Instant::now();
    let mut first_token: Option<u64> = None;
    let lane = opts.lane;
    let tag = |mut v: Value| {
        if let Some(l) = lane {
            v["lane"] = json!(l);
        }
        line(v)
    };
    let mut text = String::new();
    let (mut input, mut output) = (0u64, 0u64);
    let mut stopped = false;
    let mut error: Option<String> = None;

    match prepared.request.send().await {
        Err(e) => error = Some(format!("{model} is unreachable: {e}")),
        Ok(response) if !response.status().is_success() => {
            let status = response.status();
            let bytes = response.bytes().await.unwrap_or_default();
            let message = if prepared.openai.is_some() {
                crate::openai::translate_error(status, &bytes, &model).2
            } else {
                serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| crate::util::truncate(&String::from_utf8_lossy(&bytes), 300).to_string())
            };
            error = Some(format!("{model} answered {status}: {message}"));
        }
        Ok(response) if prepared.streaming => {
            let mut decoder = SseDecoder::default();
            let mut chunks = response.bytes_stream();
            'read: while let Some(chunk) = chunks.next().await {
                let Ok(chunk) = chunk else {
                    error = Some("the model's stream broke off".into());
                    break;
                };
                for data in decoder.feed(&chunk) {
                    let step = stream_step(&data);
                    if let Some(n) = step.input_tokens {
                        input = n;
                    }
                    if let Some(n) = step.output_tokens {
                        output = n;
                    }
                    if let Some(e) = step.error {
                        error = Some(e);
                        break 'read;
                    }
                    if let Some(t) = step.text {
                        first_token.get_or_insert(started.elapsed().as_millis() as u64);
                        text.push_str(&t);
                        if tx.send(tag(json!({"type": "delta", "text": t}))).await.is_err() {
                            stopped = true;
                            break 'read;
                        }
                    }
                }
            }
        }
        Ok(response) => {
            // The openai wire: one whole reply, translated back to the Anthropic shape.
            let bytes = response.bytes().await.unwrap_or_default();
            let translated = match &prepared.openai {
                Some(info) => crate::openai::translate_response(&bytes, info)
                    .map(|(v, _)| v)
                    .map_err(|e| e.to_string()),
                None => serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()),
            };
            match translated {
                Ok(v) => {
                    text = v["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("");
                    input = v["usage"]["input_tokens"].as_u64().unwrap_or(0);
                    output = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
                    first_token = Some(started.elapsed().as_millis() as u64);
                    if tx.send(tag(json!({"type": "delta", "text": text}))).await.is_err() {
                        stopped = true;
                    }
                }
                Err(e) => error = Some(format!("the reply did not translate: {e}")),
            }
        }
    }

    let cost = prepared.pricing.map(|p| {
        p.cost_usd(crate::providers::Usage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        })
    });
    let reply = ChatMessage {
        id: short_id(),
        role: "assistant".into(),
        content: text,
        ts: now(),
        model: Some(model.clone()),
        input_tokens: input,
        output_tokens: output,
        cost_usd: cost,
        stopped,
        error: error.clone(),
        parent_id: opts.parent_id,
        first_token_ms: first_token,
        latency_ms: Some(started.elapsed().as_millis() as u64),
        attachments: Vec::new(),
        candidate: opts.candidate,
        lane: opts.lane,
    };
    if let Err(e) = append_message(&app, &meta.id, &reply).await {
        eprintln!("chat: could not store a reply in {}: {e:#}", meta.id);
    }
    if input + output > 0 {
        let org = meta.workspace.clone().unwrap_or_else(|| CHAT_ORG.to_string());
        crate::spend::record_chat_usage(&app, &org, &model, input, output, cost).await;
    }
    // The first good reply names the conversation, when it still carries its automatic title.
    let mut titled: Option<ChatMeta> = None;
    if error.is_none() && !opts.candidate && meta.auto_title {
        let messages = read_messages(&app, &meta.id).await;
        if messages
            .iter()
            .filter(|m| m.role == "assistant" && !m.candidate && m.error.is_none())
            .count()
            == 1
            && let Ok(title) = generate_title(&app, &messages).await
            && let Some(mut current) = read_meta(&app, &meta.id).await
            && current.auto_title
        {
            current.title = title;
            current.auto_title = false;
            if write_meta(&app, &current).await.is_ok() {
                titled = Some(current);
            }
        }
    }
    let mut last = match &error {
        Some(e) => json!({"type": "error", "message": e, "message_record": reply}),
        None => json!({"type": "done", "message": reply}),
    };
    if let Some(meta) = titled {
        last["chat"] = json!(meta);
    }
    let _ = tx.send(tag(last)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_never_reaches_the_subscription_login() {
        // A plain Claude model with neither an Anthropic provider nor an API key is refused: the
        // subscription login is not a way to reach it.
        assert_eq!(resolve("claude-haiku-4-5", &[], false, false), Err(NO_CLAUDE_KEY.to_string()));
        assert_eq!(
            resolve("claude-haiku-4-5", &[], false, true),
            Ok(Route::ApiKey("claude-haiku-4-5".into()))
        );
        assert_eq!(
            resolve("claude-haiku-4-5", &[], true, true),
            Ok(Route::Provider("claude-haiku-4-5".into()))
        );
        assert_eq!(
            resolve("zai/glm-5.3-flash", &["zai"], false, false),
            Ok(Route::Provider("zai/glm-5.3-flash".into()))
        );
        assert!(resolve("nosuch/model", &["zai"], true, true).is_err());
        assert!(resolve("zai/", &["zai"], true, true).is_err());
        assert!(resolve("  ", &["zai"], true, true).is_err());
    }

    #[test]
    fn ids_are_never_paths() {
        assert!(valid_id("a1b2c3d4"));
        for bad in ["", "..", "a/b", "A1", "x".repeat(33).as_str(), "a.json"] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id: short_id(),
            role: role.into(),
            content: content.into(),
            ..ChatMessage::default()
        }
    }

    #[test]
    fn history_starts_on_a_user_turn_alternates_and_skips_errors() {
        let mut failed = msg("assistant", "partial");
        failed.error = Some("boom".into());
        let messages = vec![
            msg("assistant", "orphan reply"),
            msg("user", "first"),
            msg("assistant", "answer"),
            msg("user", "second"),
            failed,
            msg("user", "third"),
        ];
        let history = history_for_model(&messages);
        assert_eq!(history[0]["role"], "user");
        assert_eq!(history[0]["content"], "first");
        assert_eq!(history[1]["content"], "answer");
        assert_eq!(history[2]["role"], "user");
        assert_eq!(history[2]["content"], "second\n\nthird", "consecutive user turns merge");
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn the_sse_decoder_handles_split_chunks_and_the_stream_steps_read_anthropic_events() {
        let mut d = SseDecoder::default();
        let start =
            r#"{"type":"message_start","message":{"usage":{"input_tokens":12,"cache_read_input_tokens":3,"output_tokens":1}}}"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#;
        let stream = format!("event: message_start\ndata: {start}\n\nevent: content_block_delta\ndata: {delta}\n\n");
        let (a, b) = stream.split_at(40);
        let mut events = d.feed(a.as_bytes());
        events.extend(d.feed(b.as_bytes()));
        assert_eq!(events.len(), 2);
        assert_eq!(stream_step(&events[0]).input_tokens, Some(15));
        assert_eq!(stream_step(&events[1]).text.as_deref(), Some("Hel"));
        assert_eq!(
            stream_step(r#"{"type":"message_delta","usage":{"output_tokens":42}}"#).output_tokens,
            Some(42)
        );
        assert_eq!(
            stream_step(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#)
                .error
                .as_deref(),
            Some("Overloaded")
        );
        assert_eq!(stream_step("not json"), StreamStep::default());
    }

    #[test]
    fn messages_round_trip_and_a_torn_last_line_is_skipped() {
        let a = msg("user", "hi");
        let mut text = serde_json::to_string(&a).unwrap();
        text.push('\n');
        text.push_str("{\"id\":\"x\",\"role\":\"assi");
        let parsed = parse_messages(&text);
        assert_eq!(parsed, vec![a]);
    }

    #[test]
    fn attachments_are_checked_before_anything_is_read() {
        let file = |path: &str| Attachment::File {
            repo: "acme/web".into(),
            path: path.into(),
            reference: None,
        };
        assert!(check_attachment(&file("src/main.rs")).is_ok());
        for bad in ["", "/etc/passwd", "../secret", "src/../../x", "a//b", "./a"] {
            assert!(check_attachment(&file(bad)).is_err(), "{bad:?}");
        }
        let bad_repo = Attachment::File {
            repo: "not a repo".into(),
            path: "a".into(),
            reference: None,
        };
        assert!(check_attachment(&bad_repo).is_err());
        assert!(check_attachment(&Attachment::Colony { id: "../x".into() }).is_err());
        assert!(check_attachment(&Attachment::Colony { id: "abc123".into() }).is_ok());
        let snippet = |t: String| Attachment::Snippet {
            label: String::new(),
            text: t,
        };
        assert!(check_attachment(&snippet("x".repeat(FILE_CONTEXT_LIMIT + 1))).is_err());
        assert!(check_attachment(&snippet("  ".into())).is_err());
        assert!(check_attachment(&snippet("fn main() {}".into())).is_ok());
        let image = |t: &str, d: &str| Attachment::Image {
            media_type: t.into(),
            data: d.into(),
            name: String::new(),
        };
        assert!(check_attachment(&image("image/png", "iVBORw0KGgo=")).is_ok());
        assert!(check_attachment(&image("image/svg+xml", "PHN2Zz4=")).is_err());
        assert!(check_attachment(&image("image/png", "not base64!")).is_err());
        assert!(
            check_attachment(&Attachment::MergedPrs {
                org: None,
                days: Some(90)
            })
            .is_err()
        );
        // The wire shape is tagged by `kind`.
        let parsed: Attachment =
            serde_json::from_value(json!({"kind": "file", "repo": "a/b", "path": "x", "ref": "main"})).unwrap();
        assert_eq!(
            parsed,
            Attachment::File {
                repo: "a/b".into(),
                path: "x".into(),
                reference: Some("main".into())
            }
        );
    }

    #[test]
    fn forks_keep_the_messages_up_to_one_and_leave_candidates_out() {
        let (a, b, c) = (msg("user", "one"), msg("assistant", "two"), msg("user", "three"));
        let mut cand = msg("assistant", "maybe");
        cand.candidate = true;
        let all = vec![a.clone(), b.clone(), cand, c.clone()];
        assert_eq!(fork_messages(&all, &b.id, true).unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(fork_messages(&all, &c.id, false).unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(fork_messages(&all, &a.id, false).unwrap(), vec![]);
        assert!(fork_messages(&all, "nosuch", true).is_none());
    }

    #[test]
    fn picking_a_compare_reply_keeps_it_and_drops_its_sibling() {
        let user = msg("user", "q");
        let lane = |n: u8| {
            let mut m = msg("assistant", &format!("lane {n}"));
            m.candidate = true;
            m.lane = Some(n);
            m.parent_id = Some(user.id.clone());
            m
        };
        let (l0, l1) = (lane(0), lane(1));
        // Candidates stay out of the model's history until one is picked.
        assert_eq!(history_for_model(&[user.clone(), l0.clone(), l1.clone()]).len(), 1);
        let kept = pick_candidate(vec![user.clone(), l0.clone(), l1.clone()], &l1.id).unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[1].id, l1.id);
        assert!(!kept[1].candidate && kept[1].lane.is_none());
        assert_eq!(history_for_model(&kept).len(), 2);
        assert!(pick_candidate(kept, &user.id).is_err(), "only a candidate can be picked");
    }

    #[test]
    fn generated_titles_are_cleaned() {
        assert_eq!(
            clean_title("\n  \"Rust borrow checker help.\"\nmore").as_deref(),
            Some("Rust borrow checker help")
        );
        assert_eq!(
            clean_title("Title: **Release notes draft**").as_deref(),
            Some("Release notes draft")
        );
        assert_eq!(clean_title("# Why  colony   failed").as_deref(), Some("Why colony failed"));
        assert_eq!(clean_title("  \n\"\"  "), None);
        let long = clean_title(&"word ".repeat(40)).unwrap();
        assert!(long.chars().count() <= 60 && long.ends_with('…'));
    }

    #[test]
    fn issues_are_checked_before_gh_runs() {
        let issue = |repo: &str, title: &str, body: &str| NewIssue {
            repo: repo.into(),
            title: title.into(),
            body: body.into(),
        };
        assert!(check_issue(&issue("acme/web", "Fix the thing", "body")).is_ok());
        assert!(check_issue(&issue("acme", "t", "")).is_err());
        assert!(check_issue(&issue("acme/web", "  ", "")).is_err());
        assert!(check_issue(&issue("acme/web", "two\nlines", "")).is_err());
        assert!(check_issue(&issue("acme/web", &"t".repeat(257), "")).is_err());
        assert!(check_issue(&issue("acme/web", "t", &"b".repeat(FILE_CONTEXT_LIMIT + 1))).is_err());
    }

    #[test]
    fn temperatures_are_clamped() {
        assert_eq!(clamp_temperature(1.7), 1.0);
        assert_eq!(clamp_temperature(-0.2), 0.0);
        assert_eq!(clamp_temperature(f64::NAN), 1.0);
        assert_eq!(clamp_temperature(0.3), 0.3);
    }

    #[test]
    fn markdown_export_names_the_speakers_and_leaves_candidates_out() {
        let meta = ChatMeta {
            title: "Plan".into(),
            model: "zai/glm".into(),
            system: Some("Be brief".into()),
            ..ChatMeta::default()
        };
        let mut reply = msg("assistant", "Sure.");
        reply.model = Some("zai/glm".into());
        let mut user = msg("user", "Help");
        user.attachments.push(AttachmentNote {
            kind: "file".into(),
            label: "a/b/x.rs".into(),
        });
        let mut cand = msg("assistant", "CANDIDATE");
        cand.candidate = true;
        let md = to_markdown(&meta, &[user, reply, cand]);
        assert!(md.starts_with("# Plan\n"));
        assert!(md.contains("> **System:** Be brief"));
        assert!(md.contains("## You\n\n_Attached: file: a/b/x.rs_\n\nHelp"));
        assert!(md.contains("## Assistant (zai/glm)\n\nSure."));
        assert!(!md.contains("CANDIDATE"));
    }

    #[test]
    fn digests_filter_by_time_and_workspace() {
        let now = Utc::now();
        let session = |id: &str, repo: &str, hours: i64, merged: Option<i64>| crate::sessions::Session {
            id: id.into(),
            repo: repo.into(),
            issue_title: format!("title {id}"),
            updated_at: now - chrono::Duration::hours(hours),
            merged_at: merged.map(|d| now - chrono::Duration::days(d)),
            ..Default::default()
        };
        let all = vec![
            session("a", "acme/web", 2, Some(1)),
            session("b", "other/api", 3, Some(3)),
            session("c", "acme/old", 50, Some(20)),
        ];
        let today = colonies_digest(&all, None, now);
        assert!(today.contains("title a") && today.contains("title b") && !today.contains("title c"));
        let acme = colonies_digest(&all, Some("ACME"), now);
        assert!(acme.contains("title a") && !acme.contains("title b"));
        let week = merged_digest(&all, None, 7, now);
        assert!(week.contains("title a") && week.contains("title b") && !week.contains("title c"));
        assert!(merged_digest(&all, None, 31, now).contains("title c"));
    }

    #[test]
    fn map_components_are_described_with_files_and_connections() {
        let stored = json!({"map": {
            "components": [
                {"id": "api", "label": "API", "type": "service", "sources": [{"path": "src/api.rs"}]},
                {"id": "db", "label": "Database", "type": "datastore"}
            ],
            "connections": [{"from": "api", "to": "db", "label": "SQL"}]
        }});
        let (label, text) = describe_component(&stored, "api").unwrap();
        assert_eq!(label, "API");
        assert!(text.contains("src/api.rs") && text.contains("api → db (SQL)"));
        assert_eq!(describe_component(&stored, "database").unwrap().0, "Database");
        assert!(describe_component(&stored, "nosuch").is_none());
        let whole = describe_map(&stored);
        assert_eq!(whole.lines().filter(|l| l.starts_with("- ")).count(), 2);
    }
}
