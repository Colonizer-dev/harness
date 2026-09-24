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
use std::{path::PathBuf, time::Duration};

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
        "providers": app.providers().iter().map(|p| json!({"id": p.id, "name": p.name, "models": p.models})).collect::<Vec<_>>(),
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
}

/// `PATCH /api/chat/{id}`: rename, or change the model, system prompt, token limit or workspace.
pub async fn patch(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<PatchChat>) -> ApiResult<ChatMeta> {
    check_id(&id)?;
    let mut meta = load_or_404(read_meta(&app, &id).await)?;
    if let Some(title) = req.title {
        meta.title = title.trim().chars().take(120).collect();
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
    /// Optional context: a colony's summary and recent activity.
    pub context: Option<ChatContext>,
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

/// How much of an attached file goes to the model.
const FILE_CONTEXT_LIMIT: usize = 60 * 1024;

/// The Anthropic-shaped message list the model sees: the most recent history (errors and empty
/// replies left out), trimmed to [`HISTORY_MESSAGES`] and [`HISTORY_BYTES`], starting on a user turn
/// as the API requires. Pure, for the tests.
pub fn history_for_model(messages: &[ChatMessage]) -> Vec<Value> {
    let usable: Vec<&ChatMessage> = messages
        .iter()
        .filter(|m| (m.role == "user" || m.role == "assistant") && m.error.is_none() && !m.content.trim().is_empty())
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

/// `POST /api/chat/{id}/messages`: stores the operator's message, asks the conversation's model and
/// streams its reply as newline-delimited JSON; the reply is stored when it ends or is stopped.
pub async fn send(State(app): State<Shared>, Path(id): Path<String>, Json(req): Json<Send>) -> Result<Response, crate::AppError> {
    check_id(&id)?;
    let mut meta = load_or_404(read_meta(&app, &id).await)?;
    let (route, reach) = route_for(&app, &meta.model).map_err(|e| client_error(StatusCode::BAD_REQUEST, &e))?;
    let mut messages = read_messages(&app, &id).await;

    if req.regenerate {
        while messages.last().is_some_and(|m| m.role == "assistant") {
            messages.pop();
        }
        if messages.last().is_none_or(|m| m.role != "user") {
            return Err(client_error(StatusCode::BAD_REQUEST, "nothing to regenerate"));
        }
        rewrite_messages(&app, &id, &messages).await?;
    } else {
        let content = req.content.trim();
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
            ..ChatMessage::default()
        };
        append_message(&app, &id, &user).await?;
        messages.push(user);
        if meta.title.is_empty() {
            meta.title = content.lines().next().unwrap_or_default().chars().take(60).collect();
        }
    }

    let mut system = meta.system.clone().unwrap_or_default();
    let context = req.context.unwrap_or_default();
    if let Some(colony) = context.colony
        && let Some(s) = app.session(&colony).await
    {
        let recent = crate::autonomy::event_context(&app.session_dir(&s.id).join("events.jsonl")).await;
        let summary = s.summary.clone().unwrap_or_else(|| s.issue_title.clone());
        system.push_str(&format!(
            "\n\nContext — colony {} on {}: {}\nRecent activity:\n{}",
            s.id,
            s.repo,
            summary,
            recent.join("\n")
        ));
    }
    if let Some(file) = context.file
        && let Some((owner, name)) = file.repo.split_once('/')
    {
        let answer = crate::code::blob(
            State(app.clone()),
            Path((owner.to_string(), name.to_string())),
            axum::extract::Query(crate::code::PathQuery {
                path: file.path.clone(),
                reference: file.reference.clone(),
            }),
        )
        .await?;
        let text = answer.0["text"].as_str().unwrap_or_default();
        if text.is_empty() {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                "that file is binary, too large or empty",
            ));
        }
        let cut: String = text.chars().take(FILE_CONTEXT_LIMIT).collect();
        let at = answer.0["ref"].as_str().unwrap_or("HEAD");
        system.push_str(&format!(
            "\n\nContext — file {}/{} at {at}{}:\n```\n{cut}\n```",
            file.repo,
            file.path,
            if cut.len() < text.len() { " (truncated)" } else { "" }
        ));
    }
    let mut body = json!({"max_tokens": meta.max_tokens, "messages": history_for_model(&messages)});
    if !system.trim().is_empty() {
        body["system"] = json!(system.trim());
    }
    let prepared = prepare(&app, &route, &reach, body).map_err(|e| client_error(StatusCode::BAD_REQUEST, &format!("{e:#}")))?;
    meta.updated_at = now();
    write_meta(&app, &meta).await?;

    let model = route_model(&route).to_string();
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(64);
    let app2 = app.clone();
    tokio::spawn(async move { run_reply(app2, meta, model, prepared, tx).await });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|b| (Ok::<_, std::io::Error>(b), rx))
    });
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .expect("a fixed response"))
}

async fn run_reply(app: Shared, meta: ChatMeta, model: String, prepared: Prepared, tx: tokio::sync::mpsc::Sender<Bytes>) {
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
                        text.push_str(&t);
                        if tx.send(line(json!({"type": "delta", "text": t}))).await.is_err() {
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
                    if tx.send(line(json!({"type": "delta", "text": text}))).await.is_err() {
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
    };
    if let Err(e) = append_message(&app, &meta.id, &reply).await {
        eprintln!("chat: could not store a reply in {}: {e:#}", meta.id);
    }
    if input + output > 0 {
        let org = meta.workspace.clone().unwrap_or_else(|| CHAT_ORG.to_string());
        crate::spend::record_chat_usage(&app, &org, &model, input, output, cost).await;
    }
    let last = match &error {
        Some(e) => json!({"type": "error", "message": e, "message_record": reply}),
        None => json!({"type": "done", "message": reply}),
    };
    let _ = tx.send(line(last)).await;
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
}
