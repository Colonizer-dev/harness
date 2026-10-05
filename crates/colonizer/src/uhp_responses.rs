//! The task-bearing half of the UHP core (docs/protocol.md §7.3, §7.4, §7.6, issue #650):
//! `POST /uhp/v1/responses` starts a colony or continues one, a response streams as Server-Sent
//! Events projected from the colony's stored event log, and both a response and a whole session can
//! be cancelled, idempotently.
//!
//! A response is one turn of a colony. Its id, `resp_<session id>.<run epoch>.<turn>`, is derived
//! from the event log rather than stored: `<turn>` counts the run epoch's turns from 1, a turn
//! being everything up to its `turn_end`. The projection ([`Projection`]) is pure — §2 events in,
//! SSE events out — so the stored log, a live colony and a test's fake event source all read alike.

// The refusals here are finished `Response`s, returned as the `Err` of the validation steps, as
// `sessions::create` returns its `AppError`.
#![allow(clippy::result_large_err)]

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse as _, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::{broadcast, mpsc};

use crate::{
    App, AppError, Shared,
    api_tokens::ScopedToken,
    sessions::{NewSession, Runtime, Session, SessionStatus},
    uhp::{method_not_allowed, typed_envelope, uhp_error},
};

/// The routes this module serves, merged into `uhp::routes` so the version negotiation layer covers
/// them like every other served `/uhp` route.
pub(crate) fn routes() -> axum::Router<Shared> {
    axum::Router::new()
        .route("/uhp/v1/responses", post(create).fallback(method_not_allowed))
        .route("/uhp/v1/responses/{response_id}", get(retrieve).fallback(method_not_allowed))
        .route(
            "/uhp/v1/responses/{response_id}/cancel",
            post(cancel_response).fallback(method_not_allowed),
        )
        .route(
            "/uhp/v1/sessions/{id}/cancel",
            post(cancel_session).fallback(method_not_allowed),
        )
}

// ---------------------------------------------------------------------------
// Response ids
// ---------------------------------------------------------------------------

/// A parsed response id: the colony, its run epoch and the turn within it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResponseId {
    pub(crate) session: String,
    pub(crate) epoch: u64,
    pub(crate) turn: u64,
}

impl ResponseId {
    /// Parses `resp_<session>.<epoch>.<turn>`, read from the right so a session id is taken whole.
    /// Anything else — no prefix, an empty session, a zero or non-numeric epoch or turn — is `None`.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let rest = raw.strip_prefix("resp_")?;
        let mut parts = rest.rsplitn(3, '.');
        let turn = parts.next()?.parse::<u64>().ok().filter(|t| *t > 0)?;
        let epoch = parts.next()?.parse::<u64>().ok().filter(|e| *e > 0)?;
        let session = parts.next().filter(|s| !s.is_empty())?;
        Some(ResponseId {
            session: session.to_string(),
            epoch,
            turn,
        })
    }

    fn render(&self) -> String {
        format!("resp_{}.{}.{}", self.session, self.epoch, self.turn)
    }
}

/// The colony a response path names, for the scoped-token allowlist: `None` when the id does not
/// parse, which the allowlist reads as an unknown colony.
pub(crate) fn session_of(raw: &str) -> Option<String> {
    ResponseId::parse(raw).map(|id| id.session)
}

// ---------------------------------------------------------------------------
// Interrupts
// ---------------------------------------------------------------------------

/// Turns ended by an interrupt — a response cancel or the socket's `interrupt` — so their
/// `turn_end` projects as `cancelled` (§7.4). In memory: the runtime's own interrupt flag is spent
/// by the turn-end handling before a stream reads it. After a restart an interrupted turn that
/// already finished reads as failed, its `turn_end` being an error.
static INTERRUPTED: LazyLock<std::sync::Mutex<HashSet<ResponseId>>> = LazyLock::new(Default::default);

fn mark_interrupted(id: &ResponseId) {
    let mut set = INTERRUPTED.lock().unwrap_or_else(|p| p.into_inner());
    if set.len() > 4096 {
        set.clear();
    }
    set.insert(id.clone());
}

fn was_interrupted(id: &ResponseId) -> bool {
    INTERRUPTED.lock().unwrap_or_else(|p| p.into_inner()).contains(id)
}

/// Records that the colony's current turn was interrupted from the cockpit's socket, so its UHP
/// response ends `cancelled` like one cancelled over `/uhp`.
pub(crate) async fn note_interrupt(app: &App, session: &str) {
    let epoch = crate::lifecycle::run_epoch_for_dir(&app.session_dir(session));
    let shape = Shape::of(&read_epoch(app, session, epoch, epoch).await);
    mark_interrupted(&ResponseId {
        session: session.to_string(),
        epoch,
        turn: shape.ended + 1,
    });
}

// ---------------------------------------------------------------------------
// The projection: §2 events in, UHP stream events out
// ---------------------------------------------------------------------------

/// Whether a §2 event belongs to a turn's work, so the first of them opens the turn. Status changes,
/// logs, session and model announcements and policy notes happen around turns, not in one.
fn opens_turn(kind: &str) -> bool {
    matches!(
        kind,
        "user_message"
            | "assistant_text_delta"
            | "assistant_text"
            | "thinking"
            | "tool_call"
            | "tool_result"
            | "question"
            | "question_answered"
            | "memory_proposal"
            | "finding"
    )
}

/// How a response ended when its turn had no `turn_end`: what the colony's state says (§7.7).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ending {
    /// Stop while it ran: `cancelled`.
    Cancelled,
    /// A budget, the host-disk quota or the max session length: `incomplete` with this reason.
    Incomplete(&'static str),
    /// Anything else: `failed` with this code, after an `error` event.
    Failed(&'static str, String),
}

/// The ending a colony's record gives a response whose turn never reached `turn_end`.
pub(crate) fn ending_for(s: &Session) -> Ending {
    let error = s.error.clone().unwrap_or_default();
    match s.status {
        SessionStatus::Stopped if error.is_empty() => Ending::Cancelled,
        SessionStatus::Stopped if error.contains("spend budget") || error.contains("token budget") => {
            Ending::Incomplete("colonizer_spend_budget")
        }
        SessionStatus::Stopped if error.contains("host-disk quota") => Ending::Incomplete("colonizer_host_disk_quota"),
        SessionStatus::Stopped if error == crate::sessions::VM_STOPPED_EARLY => {
            Ending::Incomplete("colonizer_max_session_length")
        }
        SessionStatus::Parked => Ending::Failed("quota_exhausted", "the colony is parked until its plan's quota resets".into()),
        _ if error == crate::sessions::PUBLISH_LOST_TO_RESTART => Ending::Failed("colonizer_publish_interrupted", error),
        _ => Ending::Failed(
            "harness_error",
            if error.is_empty() {
                "the colony's runner went away before the turn ended".into()
            } else {
                error
            },
        ),
    }
}

/// The failure code a `turn_end` with `is_error: true` carries, from the colony's attention (§7.7).
fn failure_code(attention: Option<&Value>) -> &'static str {
    match attention.and_then(|a| a["reason"].as_str()) {
        Some("model_error") => "provider_error",
        Some("provider_quota_exhausted") => "quota_exhausted",
        Some("nudges_exhausted") => "colonizer_agent_stalled",
        _ => "harness_error",
    }
}

/// Token totals out of a `turn_end`'s cumulative `model_usage`: input (cache reads and writes
/// included, as the Responses API counts them), cached input, output.
fn usage_totals(model_usage: &Value) -> (u64, u64, u64) {
    let mut totals = (0, 0, 0);
    if let Some(models) = model_usage.as_object() {
        for usage in models.values() {
            let n = |key: &str| usage[key].as_u64().unwrap_or(0);
            totals.0 += n("input_tokens") + n("cache_read_tokens") + n("cache_write_tokens");
            totals.1 += n("cache_read_tokens");
            totals.2 += n("output_tokens");
        }
    }
    totals
}

/// One response's projection. Fed every stored event of its run epoch in order; it counts turns by
/// `turn_end`, keeps the cumulative totals of the turns before its own, and emits stream events only
/// for its own turn.
pub(crate) struct Projection {
    id: ResponseId,
    harness_id: String,
    created_at: i64,
    /// The turn the next event belongs to, from 1.
    turn: u64,
    started: bool,
    done: bool,
    /// The colony said `exited` inside this turn: the driver ends it from the colony's record.
    pub(crate) exited: bool,
    /// The turn was interrupted: its `turn_end` reads `cancelled`.
    pub(crate) cancelled: bool,
    /// The colony's attention when the turn ended, for a failed turn's code.
    pub(crate) attention: Option<Value>,
    sequence: u64,
    output: Vec<Value>,
    /// `(message_id, block_index)` of the message item text is streaming into, with its index.
    open_message: Option<(String, u64, usize)>,
    model: String,
    previous_cost: f64,
    previous_usage: (u64, u64, u64),
    status: &'static str,
    usage: Value,
    error: Value,
    incomplete: Value,
    metadata: Map<String, Value>,
}

impl Projection {
    pub(crate) fn new(id: ResponseId, harness_id: &str, created_at: i64) -> Self {
        let mut metadata = Map::new();
        metadata.insert("session_id".into(), json!(id.session));
        metadata.insert("harness_id".into(), json!(harness_id));
        Projection {
            id,
            harness_id: harness_id.to_string(),
            created_at,
            turn: 1,
            started: false,
            done: false,
            exited: false,
            cancelled: false,
            attention: None,
            sequence: 0,
            output: Vec::new(),
            open_message: None,
            model: String::new(),
            previous_cost: 0.0,
            previous_usage: (0, 0, 0),
            status: "in_progress",
            usage: Value::Null,
            error: Value::Null,
            incomplete: Value::Null,
            metadata,
        }
    }

    pub(crate) fn done(&self) -> bool {
        self.done
    }

    /// Whether the next event belongs to this response's turn.
    fn current(&self) -> bool {
        self.turn == self.id.turn && !self.done
    }

    /// The response as it stands: the Responses API object with UHP's metadata.
    pub(crate) fn response(&self) -> Value {
        json!({
            "id": self.id.render(),
            "object": "response",
            "created_at": self.created_at,
            "status": self.status,
            "model": if self.model.is_empty() { self.harness_id.clone() } else { self.model.clone() },
            "output": self.output,
            "usage": self.usage,
            "error": self.error,
            "incomplete_details": self.incomplete,
            "metadata": self.metadata,
        })
    }

    fn event(&mut self, kind: &str, mut body: Value) -> Value {
        body["type"] = json!(kind);
        body["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        body
    }

    /// `response.created` and `response.in_progress`, once.
    pub(crate) fn begin(&mut self) -> Vec<Value> {
        if self.started || self.done {
            return Vec::new();
        }
        self.started = true;
        let response = self.response();
        let created = self.event("response.created", json!({ "response": response }));
        let response = self.response();
        let progress = self.event("response.in_progress", json!({ "response": response }));
        vec![created, progress]
    }

    /// Closes a message item still streaming text, as when a tool call follows partial text.
    fn close_message(&mut self, out: &mut Vec<Value>) {
        if let Some((_, _, index)) = self.open_message.take() {
            self.output[index]["status"] = json!("completed");
            let item = self.output[index].clone();
            out.push(self.event("response.output_item.done", json!({ "output_index": index, "item": item })));
        }
    }

    /// The message item for one assistant text block, opened (with its `added` events) if new.
    fn message_item(&mut self, message_id: &str, block: u64, out: &mut Vec<Value>) -> usize {
        if let Some((m, b, index)) = &self.open_message
            && m == message_id
            && *b == block
        {
            return *index;
        }
        self.close_message(out);
        let index = self.output.len();
        let item = json!({
            "type": "message",
            "id": format!("msg_{message_id}_{block}"),
            "role": "assistant",
            "status": "in_progress",
            "content": [{"type": "output_text", "text": "", "annotations": []}],
        });
        self.output.push(item.clone());
        out.push(self.event("response.output_item.added", json!({ "output_index": index, "item": item })));
        let item_id = format!("msg_{message_id}_{block}");
        out.push(self.event(
            "response.content_part.added",
            json!({
                "output_index": index, "item_id": item_id, "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []},
            }),
        ));
        self.open_message = Some((message_id.to_string(), block, index));
        index
    }

    /// Feeds one stored §2 event; returns the stream events it projects to (none for another turn's).
    pub(crate) fn feed(&mut self, event: &Value) -> Vec<Value> {
        let kind = event["type"].as_str().unwrap_or_default();
        if kind == "model_changed"
            && let Some(model) = event["model"].as_str()
            && self.turn <= self.id.turn
        {
            self.model = model.to_string();
        }
        if kind == "turn_end" {
            if self.current() {
                let out = self.end_turn(event);
                self.turn += 1;
                return out;
            }
            if self.turn < self.id.turn {
                // A turn before ours: its totals are what ours is measured from (§2 makes them
                // cumulative).
                self.previous_cost = event["cost_usd"].as_f64().unwrap_or(self.previous_cost);
                if event.get("model_usage").is_some_and(Value::is_object) {
                    self.previous_usage = usage_totals(&event["model_usage"]);
                }
            }
            self.turn += 1;
            return Vec::new();
        }
        if !self.current() {
            return Vec::new();
        }
        if kind == "status" && event["state"] == "exited" {
            self.exited = true;
            return Vec::new();
        }
        if !self.started && !opens_turn(kind) {
            return Vec::new();
        }
        let mut out = self.begin();
        let agent = event.get("agent").cloned();
        match kind {
            "user_message" => {}
            "assistant_text_delta" => {
                let message = event["message_id"].as_str().unwrap_or_default();
                let block = event["block_index"].as_u64().unwrap_or(0);
                let delta = event["delta"].as_str().unwrap_or_default();
                let index = self.message_item(message, block, &mut out);
                if let Some(text) = self.output[index]["content"][0]["text"].as_str() {
                    self.output[index]["content"][0]["text"] = json!(format!("{text}{delta}"));
                }
                let item_id = self.output[index]["id"].clone();
                out.push(self.event(
                    "response.output_text.delta",
                    json!({ "output_index": index, "item_id": item_id, "content_index": 0, "delta": delta }),
                ));
            }
            "assistant_text" => {
                let message = event["message_id"].as_str().unwrap_or_default();
                let block = event["block_index"].as_u64().unwrap_or(0);
                let text = event["text"].as_str().unwrap_or_default();
                let index = self.message_item(message, block, &mut out);
                self.output[index]["content"][0]["text"] = json!(text);
                let item_id = self.output[index]["id"].clone();
                out.push(self.event(
                    "response.output_text.done",
                    json!({ "output_index": index, "item_id": item_id, "content_index": 0, "text": text }),
                ));
                out.push(self.event(
                    "response.content_part.done",
                    json!({
                        "output_index": index, "item_id": item_id, "content_index": 0,
                        "part": {"type": "output_text", "text": text, "annotations": []},
                    }),
                ));
                self.close_message(&mut out);
            }
            "thinking" => {
                self.close_message(&mut out);
                let text = event["text"].as_str().unwrap_or_default();
                let index = self.output.len();
                let id = format!(
                    "rs_{}_{}",
                    event["message_id"].as_str().unwrap_or_default(),
                    event["block_index"].as_u64().unwrap_or(0)
                );
                let mut item = json!({"type": "reasoning", "id": id, "summary": [{"type": "summary_text", "text": text}]});
                if let Some(agent) = &agent {
                    item["agent"] = agent.clone();
                }
                self.output.push(item.clone());
                let part = json!({"type": "summary_text", "text": text});
                out.push(self.event(
                    "response.reasoning_summary_part.added",
                    json!({ "output_index": index, "item_id": id, "summary_index": 0, "part": {"type": "summary_text", "text": ""} }),
                ));
                out.push(self.event(
                    "response.reasoning_summary_text.delta",
                    json!({ "output_index": index, "item_id": id, "summary_index": 0, "delta": text }),
                ));
                out.push(self.event(
                    "response.reasoning_summary_part.done",
                    json!({ "output_index": index, "item_id": id, "summary_index": 0, "part": part }),
                ));
            }
            "tool_call" => {
                self.close_message(&mut out);
                let call_id = event["tool_call_id"].as_str().unwrap_or_default();
                let index = self.output.len();
                let mut item = json!({
                    "type": "function_call",
                    "id": format!("fc_{call_id}"),
                    "call_id": call_id,
                    "name": event["name"].as_str().unwrap_or_default(),
                    "arguments": event.get("input").map(Value::to_string).unwrap_or_else(|| "{}".into()),
                    "status": "completed",
                });
                if let Some(agent) = &agent {
                    item["agent"] = agent.clone();
                }
                self.output.push(item.clone());
                let mut added = item.clone();
                added["status"] = json!("in_progress");
                out.push(self.event("response.output_item.added", json!({ "output_index": index, "item": added })));
                out.push(self.event("response.output_item.done", json!({ "output_index": index, "item": item })));
            }
            "tool_result" => {
                self.close_message(&mut out);
                let call_id = event["tool_call_id"].as_str().unwrap_or_default();
                let output = match &event["output"] {
                    Value::String(text) => text.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                let index = self.output.len();
                let mut item = json!({
                    "type": "function_call_output",
                    "id": format!("fco_{call_id}"),
                    "call_id": call_id,
                    "output": output,
                    "status": "completed",
                });
                if let Some(agent) = &agent {
                    item["agent"] = agent.clone();
                }
                self.output.push(item.clone());
                out.push(self.event("response.output_item.done", json!({ "output_index": index, "item": item })));
            }
            "question" | "question_answered" | "memory_proposal" | "finding" => {
                let mut body = event.clone();
                if let Some(map) = body.as_object_mut() {
                    map.remove("type");
                    map.remove("seq");
                }
                out.push(self.event(&format!("colonizer.{kind}"), body));
            }
            "log" if matches!(event["level"].as_str(), Some("warn" | "error")) => {
                let mut body = event.clone();
                if let Some(map) = body.as_object_mut() {
                    map.remove("type");
                    map.remove("seq");
                }
                out.push(self.event("colonizer.log", body));
            }
            _ => {}
        }
        out
    }

    /// The terminal event for this turn's `turn_end` (§7.4).
    fn end_turn(&mut self, event: &Value) -> Vec<Value> {
        let mut out = self.begin();
        self.close_message(&mut out);
        let cost = event["cost_usd"].as_f64();
        if let Some(cost) = cost {
            let turn_cost = (cost - self.previous_cost).max(0.0);
            self.metadata.insert("cost_usd".into(), json!(format!("{turn_cost:.6}")));
        }
        if let Some(ms) = event["duration_ms"].as_u64() {
            self.metadata.insert("duration_ms".into(), json!(ms.to_string()));
        }
        if event.get("model_usage").is_some_and(Value::is_object) {
            let now = usage_totals(&event["model_usage"]);
            let (input, cached, output) = (
                now.0.saturating_sub(self.previous_usage.0),
                now.1.saturating_sub(self.previous_usage.1),
                now.2.saturating_sub(self.previous_usage.2),
            );
            self.usage = json!({
                "input_tokens": input,
                "input_tokens_details": {"cached_tokens": cached},
                "output_tokens": output,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": input + output,
            });
        }
        self.done = true;
        if self.cancelled {
            self.status = "cancelled";
            let response = self.response();
            out.push(self.event("response.failed", json!({ "response": response })));
        } else if event["is_error"].as_bool().unwrap_or(false) {
            self.status = "failed";
            let code = failure_code(self.attention.as_ref());
            let message = event["result"].as_str().unwrap_or("the turn ended in an error").to_string();
            self.error = json!({ "code": code, "message": message });
            let response = self.response();
            out.push(self.event("response.failed", json!({ "response": response })));
        } else {
            self.status = "completed";
            let response = self.response();
            out.push(self.event("response.completed", json!({ "response": response })));
        }
        out
    }

    /// Ends a response whose turn will get no `turn_end`, by how the colony stopped (§7.4, §7.7).
    pub(crate) fn finish(&mut self, ending: Ending) -> Vec<Value> {
        if self.done {
            return Vec::new();
        }
        let mut out = self.begin();
        self.close_message(&mut out);
        self.done = true;
        match ending {
            Ending::Cancelled => {
                self.status = "cancelled";
                let response = self.response();
                out.push(self.event("response.failed", json!({ "response": response })));
            }
            Ending::Incomplete(reason) => {
                self.status = "incomplete";
                self.incomplete = json!({ "reason": reason });
                let response = self.response();
                out.push(self.event("response.incomplete", json!({ "response": response })));
            }
            Ending::Failed(code, message) => {
                self.status = "failed";
                self.error = json!({ "code": code, "message": message });
                out.push(self.event("error", json!({ "code": code, "message": message, "param": null })));
                let response = self.response();
                out.push(self.event("response.failed", json!({ "response": response })));
            }
        }
        out
    }

    /// A response cancelled before its `turn_end` arrives: the reply a cancel gives now, while the
    /// stream still ends on the turn's own `turn_end` (which reads `cancelled` too).
    fn cancelled_view(&self) -> Value {
        let mut response = self.response();
        response["status"] = json!("cancelled");
        response
    }

    /// Attention while the response runs (§7.7).
    fn note_attention(&mut self, attention: Option<&Value>) {
        match attention.and_then(|a| a["reason"].as_str()) {
            Some(reason) if !self.done => {
                self.metadata.insert("colonizer_attention".into(), json!(reason));
            }
            _ => {
                self.metadata.remove("colonizer_attention");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reading the log
// ---------------------------------------------------------------------------

/// The stored events of one run epoch, each with its `seq`: `events.jsonl` for the current epoch,
/// the archive `events-<epoch>.jsonl` for an earlier one. A line that does not decode is skipped,
/// as the socket's replay skips it.
async fn read_epoch(app: &App, session: &str, epoch: u64, current: u64) -> Vec<(u64, Value)> {
    let dir = app.session_dir(session);
    let path = if epoch == current {
        dir.join("events.jsonl")
    } else {
        dir.join(format!("events-{epoch}.jsonl"))
    };
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return Vec::new();
    };
    bytes
        .split(|b| *b == b'\n')
        .filter(|chunk| !chunk.is_empty())
        .filter_map(crate::sessions::replay_line)
        .filter_map(|(seq, line)| serde_json::from_str::<Value>(line).ok().map(|v| (seq, v)))
        .collect()
}

/// A run epoch's turn shape: how many turns ended, and whether one has opened since the last end.
#[derive(Debug, Default, Clone, Copy)]
struct Shape {
    ended: u64,
    open: bool,
}

impl Shape {
    fn of(events: &[(u64, Value)]) -> Self {
        let mut shape = Shape::default();
        for (_, event) in events {
            let kind = event["type"].as_str().unwrap_or_default();
            if kind == "turn_end" {
                shape.ended += 1;
                shape.open = false;
            } else if opens_turn(kind) {
                shape.open = true;
            }
        }
        shape
    }

    /// The epoch's latest turn: the open one, else the last ended one; a fresh epoch's first turn
    /// is its latest before any event arrives.
    fn latest(self) -> u64 {
        if self.open || self.ended == 0 {
            self.ended + 1
        } else {
            self.ended
        }
    }
}

/// A response id resolved against its colony.
struct Resolved {
    id: ResponseId,
    session: Session,
    current: u64,
    events: Vec<(u64, Value)>,
}

/// Resolves a response id, or `None` for an unknown or malformed one — or one outside a scoped
/// token's limits, which reads the same.
async fn resolve(app: &App, raw: &str, scoped: Option<&ScopedToken>) -> Option<Resolved> {
    resolve_with(app, raw, scoped, false).await
}

/// [`resolve`], optionally trusting that the turn exists: a response this server just started is
/// real before its first event reaches the log (a follow-up is still on its way to the runner).
async fn resolve_with(app: &App, raw: &str, scoped: Option<&ScopedToken>, started: bool) -> Option<Resolved> {
    let id = ResponseId::parse(raw)?;
    let session = app.session(&id.session).await?;
    if scoped.is_some_and(|token| !token.covers(&session.org, &session.repo)) {
        return None;
    }
    let current = crate::lifecycle::run_epoch_for_dir(&app.session_dir(&id.session));
    if id.epoch > current {
        return None;
    }
    let events = read_epoch(app, &id.session, id.epoch, current).await;
    let shape = Shape::of(&events);
    let exists = if id.epoch < current {
        id.turn <= shape.ended.max(shape.latest())
    } else {
        id.turn <= shape.latest()
    };
    (exists || started).then_some(Resolved {
        id,
        session,
        current,
        events,
    })
}

/// Projects a resolved response over its stored events; a turn the colony can no longer finish
/// (an earlier epoch, or a colony that is no longer running) is ended from the colony's record.
fn project(resolved: &Resolved) -> Projection {
    let mut projection = Projection::new(
        resolved.id.clone(),
        &resolved.session.agent,
        resolved.session.created_at.timestamp(),
    );
    projection.attention = resolved.session.attention.clone();
    for (_, event) in &resolved.events {
        if event["type"] == "turn_end" {
            projection.cancelled |= was_interrupted(&resolved.id);
        }
        projection.feed(event);
    }
    let running = resolved.id.epoch == resolved.current && still_running(resolved.session.status);
    if !projection.done() && (projection.exited || !running) {
        projection.finish(ending_for(&resolved.session));
    }
    projection.note_attention(resolved.session.attention.as_ref());
    projection
}

/// Whether a colony in this state may still add to its current turn.
fn still_running(status: SessionStatus) -> bool {
    status.is_live() || status == SessionStatus::Queued
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A request the route cannot take: `invalid_input`, with `param` naming the offending field as
/// the Responses API does.
fn invalid(message: impl std::fmt::Display, param: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": {
                "type": "invalid_request_error",
                "code": "invalid_input",
                "message": message.to_string(),
                "param": param,
                "detail": null,
            }
        })),
    )
        .into_response()
}

fn response_not_found() -> Response {
    uhp_error(StatusCode::NOT_FOUND, "response_not_found", "no such response", None)
}

/// A refusal from a Colonizer handler this surface runs underneath (create, resume, stop), in the
/// §7.7 envelope: its status picks the class.
fn from_app_error(error: AppError) -> Response {
    let AppError(status, message) = error;
    let message = format!("{message:#}");
    match status {
        StatusCode::BAD_REQUEST => uhp_error(status, "invalid_input", message, None),
        StatusCode::FORBIDDEN => typed_envelope(status, "permission_error", "insufficient_scope", message, Value::Null),
        StatusCode::NOT_FOUND => uhp_error(status, "session_not_found", message, None),
        StatusCode::CONFLICT => uhp_error(status, "colonizer_conflict", message, None),
        StatusCode::TOO_MANY_REQUESTS => typed_envelope(status, "rate_limit_error", "rate_limited", message, Value::Null),
        _ if status.is_server_error() => uhp_error(status, "server_error", message, None),
        _ => uhp_error(status, "invalid_input", message, None),
    }
}

// ---------------------------------------------------------------------------
// POST /uhp/v1/responses
// ---------------------------------------------------------------------------

/// The request fields this route reads; everything else is ignored and listed (§7.1).
const READ_FIELDS: &[&str] = &["input", "stream", "previous_response_id", "metadata"];

/// The metadata keys a create reads.
const METADATA_KEYS: &[&str] = &["repo", "issue", "title", "harness_id"];

/// What a create asks for, validated.
struct Ask {
    input: String,
    stream: bool,
    previous: Option<String>,
    metadata: Map<String, Value>,
    ignored: Vec<String>,
}

/// The task text out of `input`: a string, or a list of `message` items (or bare content parts)
/// whose `input_text` parts are joined. A file part is refused: discovery reports `files_input`
/// false.
fn input_text(input: &Value) -> Result<String, Response> {
    fn parts(content: &Value, out: &mut Vec<String>) -> Result<(), Response> {
        match content {
            Value::String(text) => out.push(text.clone()),
            Value::Array(items) => {
                for part in items {
                    match part["type"].as_str() {
                        Some("input_text") | Some("text") => out.push(part["text"].as_str().unwrap_or_default().to_string()),
                        Some("input_file") | Some("input_image") => {
                            return Err(invalid(
                                "file and image inputs are not served yet (discovery reports files_input false)",
                                "input",
                            ));
                        }
                        Some("message") => parts(&part["content"], out)?,
                        _ => return Err(invalid("each input item is a message or an input_text part", "input")),
                    }
                }
            }
            _ => return Err(invalid("input is a string or a list of input items", "input")),
        }
        Ok(())
    }
    let mut out = Vec::new();
    parts(input, &mut out)?;
    Ok(out.join("\n\n").trim().to_string())
}

fn parse_ask(body: &Bytes) -> Result<Ask, Response> {
    let value: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).map_err(|e| invalid(format!("the body is not JSON: {e}"), "body"))?
    };
    let Some(object) = value.as_object() else {
        return Err(invalid("the body is a JSON object", "body"));
    };
    let mut ignored: Vec<String> = object
        .keys()
        .filter(|key| !READ_FIELDS.contains(&key.as_str()))
        .cloned()
        .collect();
    let stream = match object.get("stream") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(on)) => *on,
        Some(_) => return Err(invalid("stream is a boolean", "stream")),
    };
    let previous = match object.get("previous_response_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id.clone()),
        Some(_) => return Err(invalid("previous_response_id is a string", "previous_response_id")),
    };
    let metadata = match object.get("metadata") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => {
            if map.values().any(|v| !v.is_string()) {
                return Err(invalid("metadata values are strings", "metadata"));
            }
            for key in map.keys().filter(|k| !METADATA_KEYS.contains(&k.as_str())) {
                ignored.push(format!("metadata.{key}"));
            }
            map.clone()
        }
        Some(_) => return Err(invalid("metadata is an object of strings", "metadata")),
    };
    let input = match object.get("input") {
        None | Some(Value::Null) => String::new(),
        Some(input) => input_text(input)?,
    };
    if input.is_empty() {
        return Err(invalid("input names the task: a non-empty string or input_text", "input"));
    }
    if input.len() > 100_000 {
        return Err(invalid("input is at most 100,000 characters", "input"));
    }
    ignored.sort();
    Ok(Ask {
        input,
        stream,
        previous,
        metadata,
        ignored,
    })
}

/// The metadata string at `key`, trimmed, if set and non-empty.
fn meta<'a>(metadata: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// A `harness_id` as a module id: `chrn_<module>` or the bare module id.
fn harness_module(raw: &str) -> &str {
    raw.strip_prefix("chrn_").unwrap_or(raw)
}

/// Idempotency keys (§7.3): a repeat create with the same key from the same caller within 24 hours
/// answers the first response and starts nothing. The lock is held across the create, so a repeat
/// racing a still-booting first one waits for it rather than starting a second colony.
static IDEMPOTENT: LazyLock<tokio::sync::Mutex<HashMap<String, (String, Instant)>>> = LazyLock::new(Default::default);
const IDEMPOTENCY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

async fn create(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<ScopedToken>>,
    via: Option<axum::Extension<crate::auth::Via>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let scoped = scoped.map(|axum::Extension(token)| token);
    let via = via.map(|axum::Extension(via)| via);
    let ask = match parse_ask(&body) {
        Ok(ask) => ask,
        Err(refusal) => return refusal,
    };
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| format!("{}:{k}", scoped.as_ref().map_or("owner", |t| t.id.as_str())));
    let mut idempotent = match &key {
        Some(_) => Some(IDEMPOTENT.lock().await),
        None => None,
    };
    if let (Some(map), Some(key)) = (idempotent.as_mut(), key.as_ref()) {
        map.retain(|_, (_, at)| at.elapsed() < IDEMPOTENCY_WINDOW);
        if let Some((first, _)) = map.get(key).cloned() {
            drop(idempotent);
            return answer(&app, &first, ask.stream, scoped.as_ref(), &ask.ignored).await;
        }
    }
    let started = match ask.previous.as_deref() {
        Some(previous) => continue_colony(&app, previous, &ask, scoped.as_ref(), via).await,
        None => launch(&app, &ask, scoped.clone(), via).await,
    };
    let id = match started {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    if let (Some(map), Some(key)) = (idempotent.as_mut(), key) {
        map.insert(key, (id.clone(), Instant::now()));
    }
    drop(idempotent);
    answer(&app, &id, ask.stream, scoped.as_ref(), &ask.ignored).await
}

/// The reply to a create: the response object, or its stream.
async fn answer(app: &Shared, id: &str, stream: bool, scoped: Option<&ScopedToken>, ignored: &[String]) -> Response {
    let Some(resolved) = resolve_with(app, id, scoped, true).await else {
        return response_not_found();
    };
    if stream {
        return sse(app.clone(), resolved, true).await;
    }
    let mut response = project(&resolved).response();
    if !ignored.is_empty() {
        response["metadata"]["ignored_fields"] = json!(ignored.join(","));
    }
    Json(response).into_response()
}

/// A new colony for the request: `POST /api/sessions` underneath, with the task text as its
/// instructions. Its first response is turn 1 of run epoch 1.
async fn launch(app: &Shared, ask: &Ask, scoped: Option<ScopedToken>, via: Option<crate::auth::Via>) -> Result<String, Response> {
    let Some(repo) = meta(&ask.metadata, "repo") else {
        return Err(invalid(
            "metadata.repo names the repository a new colony works on (owner/name)",
            "metadata.repo",
        ));
    };
    if !crate::util::valid_repo(repo) {
        return Err(invalid("metadata.repo is owner/name", "metadata.repo"));
    }
    let issue = match meta(&ask.metadata, "issue") {
        None => None,
        Some(raw) => match raw.trim_start_matches('#').parse::<u64>() {
            Ok(n) if n > 0 => Some(n),
            _ => return Err(invalid("metadata.issue is an issue number", "metadata.issue")),
        },
    };
    if let Some(harness) = meta(&ask.metadata, "harness_id") {
        let wanted = harness_module(harness);
        let modules = app.modules.read().await.clone();
        let owner = repo.split('/').next().unwrap_or_default();
        let effective = crate::orgs::effective_agent_module(&app.org_settings(owner), &modules);
        let installed = app.agents.iter().any(|a| a.id == wanted);
        let switched_off = !modules.agent.enabled && modules.agent.provider == wanted;
        if !installed || switched_off {
            return Err(uhp_error(StatusCode::NOT_FOUND, "harness_not_found", "no such harness", None));
        }
        if effective != wanted {
            return Err(uhp_error(
                StatusCode::CONFLICT,
                "harness_mismatch",
                format!("colonies on {repo} launch on chrn_{effective}"),
                Some(json!({ "harness_id": format!("chrn_{effective}") })),
            ));
        }
    }
    let request = NewSession {
        repo: repo.to_string(),
        issue,
        title: meta(&ask.metadata, "title").unwrap_or_default().to_string(),
        instructions: ask.input.clone(),
        ..NewSession::default()
    };
    let session = crate::sessions::create(State(app.clone()), scoped.map(axum::Extension), Json(request))
        .await
        .map_err(from_app_error)?
        .0;
    let mut entry = crate::activity::Entry::new("colony.launch", "you").colony(&session);
    entry.via = crate::activity::via_name(via);
    crate::activity::record(app, entry).await;
    Ok(ResponseId {
        session: session.id,
        epoch: 1,
        turn: 1,
    }
    .render())
}

/// A continuation (§7.3): `previous_response_id` resolved by the colony's state.
async fn continue_colony(
    app: &Shared,
    previous: &str,
    ask: &Ask,
    scoped: Option<&ScopedToken>,
    via: Option<crate::auth::Via>,
) -> Result<String, Response> {
    let Some(resolved) = resolve(app, previous, scoped).await else {
        return Err(response_not_found());
    };
    let session = &resolved.session;
    if let Some(harness) = meta(&ask.metadata, "harness_id")
        && harness_module(harness) != session.agent
    {
        return Err(uhp_error(
            StatusCode::CONFLICT,
            "harness_mismatch",
            "a colony keeps one harness for life",
            Some(json!({ "harness_id": format!("chrn_{}", session.agent) })),
        ));
    }
    let current_events = if resolved.id.epoch == resolved.current {
        resolved.events.clone()
    } else {
        read_epoch(app, &session.id, resolved.current, resolved.current).await
    };
    let shape = Shape::of(&current_events);
    let latest = ResponseId {
        session: session.id.clone(),
        epoch: resolved.current,
        turn: shape.latest(),
    };
    if resolved.id != latest {
        return Err(uhp_error(
            StatusCode::CONFLICT,
            "colonizer_not_latest",
            "a colony cannot fork: continue its latest response",
            Some(json!({ "latest_response_id": latest.render() })),
        ));
    }
    match session.status {
        SessionStatus::Idle => {
            let rt = app.runtime(&session.id).await;
            match crate::sessions::submit_message(app, &session.id, &rt, &ask.input, None, scoped).await {
                Ok(_) => Ok(ResponseId {
                    session: session.id.clone(),
                    epoch: resolved.current,
                    turn: shape.ended + 1,
                }
                .render()),
                Err(crate::sessions::MessageError::Invalid) => Err(invalid("input is at most 100,000 characters", "input")),
                Err(_) => Err(busy()),
            }
        }
        SessionStatus::Stopped | SessionStatus::Failed if !session.cleaned_up => {
            // Resumed as by `POST …/resume`, with the input riding the resume brief.
            let note = match scoped {
                Some(token) => format!("[external input from API token \"{}\"] {}", token.name, ask.input),
                None => ask.input.clone(),
            };
            app.update_session(&session.id, |x| x.resume_note = Some(note)).await;
            let resumed = crate::lifecycle::resume(State(app.clone()), Path(session.id.clone()), via.map(axum::Extension)).await;
            match resumed {
                Ok(_) => {
                    let epoch = crate::lifecycle::run_epoch_for_dir(&app.session_dir(&session.id));
                    Ok(ResponseId {
                        session: session.id.clone(),
                        epoch,
                        turn: 1,
                    }
                    .render())
                }
                Err(error) => {
                    app.update_session(&session.id, |x| x.resume_note = None).await;
                    if error.0 == StatusCode::CONFLICT {
                        Err(expired())
                    } else {
                        Err(from_app_error(error))
                    }
                }
            }
        }
        status if status.busy() || matches!(status, SessionStatus::Queued | SessionStatus::Parked) => Err(busy()),
        _ => Err(expired()),
    }
}

fn busy() -> Response {
    uhp_error(
        StatusCode::CONFLICT,
        "session_busy",
        "the colony is working; continue it once its current work ends",
        None,
    )
}

fn expired() -> Response {
    uhp_error(
        StatusCode::NOT_FOUND,
        "session_expired",
        "this colony is finished and cannot be continued",
        None,
    )
}

// ---------------------------------------------------------------------------
// GET /uhp/v1/responses/{id}
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct RetrieveQuery {
    #[serde(default)]
    stream: Option<bool>,
}

/// `GET /uhp/v1/responses/{id}` — the response as it stands, or with `?stream=true` its stream from
/// the start (or after `Last-Event-ID`).
async fn retrieve(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<ScopedToken>>,
    Path(response_id): Path<String>,
    Query(query): Query<RetrieveQuery>,
    headers: HeaderMap,
) -> Response {
    let scoped = scoped.map(|axum::Extension(token)| token);
    let Some(resolved) = resolve(&app, &response_id, scoped.as_ref()).await else {
        return response_not_found();
    };
    if query.stream.unwrap_or(false) {
        let after = headers
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split_once('.'))
            .and_then(|(epoch, seq)| Some((epoch.parse::<u64>().ok()?, seq.parse::<u64>().ok()?)))
            .filter(|(epoch, _)| *epoch == resolved.id.epoch)
            .map(|(_, seq)| seq);
        return sse_after(app, resolved, false, after).await;
    }
    Json(project(&resolved).response()).into_response()
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

async fn sse(app: Shared, resolved: Resolved, begin_now: bool) -> Response {
    sse_after(app, resolved, begin_now, None).await
}

/// The SSE stream of one response (§7.4): the stored events of its turn, then the live ones as the
/// colony appends them, until the turn's terminal event. `after` (from `Last-Event-ID`) skips the
/// stream events of stored lines up to that seq; the projection still reads them, so the stream
/// resumes where it left off.
async fn sse_after(app: Shared, resolved: Resolved, begin_now: bool, after: Option<u64>) -> Response {
    let (tx, rx) = mpsc::channel::<(String, Value)>(256);
    tokio::spawn(pump(app, resolved, begin_now, after, tx));
    // `epoch.seq` of the last stored line read rides as each event's id, for `Last-Event-ID`.
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        let (id, event) = rx.recv().await?;
        let kind = event["type"].as_str().unwrap_or("message").to_string();
        let sse = Event::default().event(kind).id(id).data(event.to_string());
        Some((Ok::<_, Infallible>(sse), rx))
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// How often a stream re-reads the colony's record, to end a turn whose colony stopped without
/// saying `exited`.
const STATE_POLL: Duration = Duration::from_secs(2);

/// Feeds the projection from the stored log and then the colony's live events into `tx`, ending at
/// the terminal event, when the colony stops, or when the client goes away.
async fn pump(app: Shared, resolved: Resolved, begin_now: bool, after: Option<u64>, tx: mpsc::Sender<(String, Value)>) {
    let id = resolved.id.clone();
    let live = id.epoch == resolved.current;
    let rt: Option<Arc<Runtime>> = if live { Some(app.runtime(&id.session).await) } else { None };
    // Subscribed before the log is read, as the socket does, so nothing lands in between.
    let mut subscription = rt.as_ref().map(|rt| rt.events.subscribe());
    let mut retired = rt.as_ref().map(|rt| rt.retired.subscribe());
    let mut projection = Projection::new(id.clone(), &resolved.session.agent, resolved.session.created_at.timestamp());
    projection.attention = resolved.session.attention.clone();
    let mut last_seq = 0u64;
    let send = |seq: u64, events: Vec<Value>| {
        let tx = tx.clone();
        let tag = format!("{}.{seq}", id.epoch);
        async move {
            for event in events {
                if tx.send((tag.clone(), event)).await.is_err() {
                    return false;
                }
            }
            true
        }
    };
    if begin_now && !send(0, projection.begin()).await {
        return;
    }
    // Replay: the stored turn, with `after` skipping what a reconnecting client already has.
    let events = if live {
        read_epoch(&app, &id.session, id.epoch, resolved.current).await
    } else {
        resolved.events
    };
    for (seq, event) in events {
        let projected = feed(&mut projection, &app, &id, &event).await;
        last_seq = seq;
        if after.is_some_and(|after| seq <= after) {
            continue;
        }
        if !send(seq, projected).await {
            return;
        }
        if projection.done() {
            return;
        }
    }
    let (Some(subscription), Some(retired)) = (subscription.as_mut(), retired.as_mut()) else {
        // An earlier run epoch: nothing more will be appended to it.
        if !projection.done() {
            let ending = app
                .session(&id.session)
                .await
                .map(|s| ending_for(&s))
                .unwrap_or(Ending::Cancelled);
            send(last_seq, projection.finish(ending)).await;
        }
        return;
    };
    if projection.exited {
        let ending = app
            .session(&id.session)
            .await
            .map(|s| ending_for(&s))
            .unwrap_or(Ending::Cancelled);
        send(last_seq, projection.finish(ending)).await;
        return;
    }
    let mut poll = tokio::time::interval(STATE_POLL);
    poll.tick().await;
    loop {
        tokio::select! {
            item = subscription.recv() => match item {
                Ok(item) => {
                    let Some(seq) = item.seq else { continue };
                    if seq <= last_seq {
                        continue;
                    }
                    let Ok(event) = serde_json::from_str::<Value>(&item.json) else { continue };
                    last_seq = seq;
                    let projected = feed(&mut projection, &app, &id, &event).await;
                    if !send(seq, projected).await || projection.done() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Too slow for the live feed: catch up from the stored log instead.
                    for (seq, event) in read_epoch(&app, &id.session, id.epoch, id.epoch).await {
                        if seq <= last_seq {
                            continue;
                        }
                        last_seq = seq;
                        let projected = feed(&mut projection, &app, &id, &event).await;
                        if !send(seq, projected).await || projection.done() {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = retired.changed() => break,
            _ = poll.tick() => {
                let Some(s) = app.session(&id.session).await else { break };
                if !still_running(s.status) {
                    break;
                }
                if tx.is_closed() {
                    return;
                }
            },
        }
        if projection.exited {
            break;
        }
    }
    // The run ended under the stream: pick up any last stored lines, then end it by the record.
    for (seq, event) in read_epoch(&app, &id.session, id.epoch, id.epoch).await {
        if seq <= last_seq {
            continue;
        }
        last_seq = seq;
        let projected = feed(&mut projection, &app, &id, &event).await;
        if !send(seq, projected).await || projection.done() {
            return;
        }
    }
    if !projection.done() {
        let ending = app
            .session(&id.session)
            .await
            .map(|s| ending_for(&s))
            .unwrap_or(Ending::Cancelled);
        send(last_seq, projection.finish(ending)).await;
    }
}

/// One event into the projection, with the interrupt mark and the colony's attention read as its
/// turn ends.
async fn feed(projection: &mut Projection, app: &App, id: &ResponseId, event: &Value) -> Vec<Value> {
    if event["type"] == "turn_end" {
        projection.cancelled |= was_interrupted(id);
        if let Some(s) = app.session(&id.session).await {
            projection.attention = s.attention.clone();
        }
    }
    projection.feed(event)
}

// ---------------------------------------------------------------------------
// Cancellation (§7.6)
// ---------------------------------------------------------------------------

/// `POST /uhp/v1/responses/{id}/cancel` — ends that turn if it has not finished: interrupted while
/// it runs (the colony and its output are kept), the colony stopped while it is still queued or
/// booting. A finished response comes back unchanged, so a repeat is never an error.
async fn cancel_response(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<ScopedToken>>,
    Path(response_id): Path<String>,
) -> Response {
    let scoped = scoped.map(|axum::Extension(token)| token);
    let Some(resolved) = resolve(&app, &response_id, scoped.as_ref()).await else {
        return response_not_found();
    };
    let projection = project(&resolved);
    if projection.done() || resolved.id.epoch != resolved.current {
        return Json(projection.response()).into_response();
    }
    match resolved.session.status {
        SessionStatus::Queued | SessionStatus::Starting => {
            if let Err(error) = crate::lifecycle::stop(State(app.clone()), Path(resolved.id.session.clone())).await {
                return from_app_error(error);
            }
            match resolve(&app, &response_id, None).await {
                Some(after) => Json(project(&after).response()).into_response(),
                None => response_not_found(),
            }
        }
        SessionStatus::Running | SessionStatus::WaitingForAnswer | SessionStatus::Idle => {
            mark_interrupted(&resolved.id);
            let rt = app.runtime(&resolved.id.session).await;
            rt.interrupted.store(true, std::sync::atomic::Ordering::SeqCst);
            rt.send_command(json!({"type": "interrupt"}));
            app.session_log(
                &resolved.id.session,
                "info",
                format!("interrupted the turn: UHP cancel of {}", resolved.id.render()),
            )
            .await;
            Json(projection.cancelled_view()).into_response()
        }
        _ => Json(projection.response()).into_response(),
    }
}

/// `POST /uhp/v1/sessions/{id}/cancel` — the stop handler under its UHP name: `{id, status}`, the
/// status from §7.7. A colony already over answers its standing status, never an error; one still
/// publishing is `session_busy` until the push settles.
async fn cancel_session(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<ScopedToken>>,
    Path(id): Path<String>,
) -> Response {
    let scoped = scoped.map(|axum::Extension(token)| token);
    let visible = app
        .session(&id)
        .await
        .filter(|s| scoped.as_ref().is_none_or(|token| token.covers(&s.org, &s.repo)));
    if visible.is_none() {
        return uhp_error(StatusCode::NOT_FOUND, "session_not_found", "no such session", None);
    }
    match crate::lifecycle::stop(State(app.clone()), Path(id.clone())).await {
        Ok(Json(reply)) => {
            let result = reply.result;
            let status = match reply.session.status {
                SessionStatus::Stopped if reply.session.error.is_none() => "cancelled",
                SessionStatus::Stopped => match ending_for(&reply.session) {
                    Ending::Incomplete(_) => "incomplete",
                    _ => "failed",
                },
                SessionStatus::Failed => "failed",
                SessionStatus::PrOpened | SessionStatus::Merged | SessionStatus::NoChanges | SessionStatus::Closed => "completed",
                _ => "cancelled",
            };
            Json(json!({
                "id": id,
                "object": "session",
                "status": status,
                "metadata": {"colonizer_result": serde_json::to_value(result).unwrap_or(Value::Null)},
            }))
            .into_response()
        }
        Err(error) if error.0 == StatusCode::CONFLICT => busy(),
        Err(error) if error.0 == StatusCode::NOT_FOUND => {
            uhp_error(StatusCode::NOT_FOUND, "session_not_found", "no such session", None)
        }
        Err(error) => from_app_error(error),
    }
}

#[cfg(test)]
mod tests;
