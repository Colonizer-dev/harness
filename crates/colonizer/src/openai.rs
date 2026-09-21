//! The `openai` wire (docs/protocol.md §6.5): translation between the Anthropic Messages API, which
//! colonies speak, and OpenAI's Chat Completions API. The gateway translates the request on the way out
//! and wraps the upstream body on the way back, so nothing inside a colony knows the provider is not
//! Anthropic-compatible. The colony's fallback re-sends its own untranslated body to Anthropic
//! (router.mjs), so translating here leaves the fallback path untouched.
//!
//! Requests are rebuilt from an allowlist. Claude Code sends fields OpenAI rejects (`thinking`,
//! `context_management`, `output_config`, `metadata`, `cache_control` on any block), and sampling
//! parameters (`temperature`, `top_p`, `stop_sequences`) that OpenAI's reasoning models refuse unless left
//! at their defaults, so anything not mapped here is dropped rather than forwarded.

use crate::providers::Usage;
use axum::{body::Bytes, http::StatusCode};
use futures_util::{Stream, StreamExt};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

/// Longest SSE line the stream translation buffers before giving up on the provider.
const MAX_LINE: usize = 8 * 1024 * 1024;
const IMAGE_NOTE: &str = "(image output attached below)";

/// The only endpoint the wire translates. `count_tokens` has no OpenAI equivalent: the gateway answers it
/// with 404, and the colony router estimates the count instead.
pub fn upstream_path(rest: &str) -> Option<&'static str> {
    (rest == "/v1/messages").then_some("/v1/chat/completions")
}

/// What translating the response needs to know about the request.
#[derive(Clone, Debug)]
pub struct RequestInfo {
    pub model: String,
    pub stream: bool,
}

pub fn translate_request(body: &[u8]) -> Result<(Vec<u8>, RequestInfo), String> {
    let request: Value = serde_json::from_slice(body).map_err(|e| format!("request body is not JSON: {e}"))?;
    let model = request["model"]
        .as_str()
        .filter(|m| !m.is_empty())
        .ok_or("request has no model")?
        .to_string();
    let stream = request["stream"].as_bool().unwrap_or(false);

    let mut messages = Vec::new();
    let system = text_of(&request["system"]);
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in request["messages"].as_array().ok_or("request has no messages")? {
        match message["role"].as_str() {
            Some("user") => user_messages(&message["content"], &mut messages),
            Some("assistant") => assistant_message(&message["content"], &mut messages),
            // Claude Code puts system messages mid-conversation (its `<system-reminder>` environment and
            // budget notes), as a string or as blocks with `cache_control`. OpenAI takes them anywhere too.
            Some("system") => {
                let text = text_of(&message["content"]);
                if !text.is_empty() {
                    messages.push(json!({"role": "system", "content": text}));
                }
            }
            _ => {}
        }
    }

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("messages".into(), Value::Array(messages));
    if let Some(max_tokens) = request["max_tokens"].as_u64() {
        out.insert("max_completion_tokens".into(), json!(max_tokens));
    }
    // Anthropic server tools (web search and the like) have no `input_schema` and run on Anthropic's side.
    let functions: Vec<Value> = request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|tool| tool["input_schema"].is_object())
        .filter_map(|tool| {
            let mut function = json!({"name": tool["name"].as_str()?, "parameters": tool["input_schema"]});
            if let Some(description) = tool["description"].as_str() {
                function["description"] = json!(description);
            }
            Some(json!({"type": "function", "function": function}))
        })
        .collect();
    if !functions.is_empty() {
        out.insert("tools".into(), Value::Array(functions));
        let choice = &request["tool_choice"];
        let mapped = match choice["type"].as_str() {
            Some("auto") => Some(json!("auto")),
            Some("any") => Some(json!("required")),
            Some("none") => Some(json!("none")),
            Some("tool") => choice["name"]
                .as_str()
                .map(|name| json!({"type": "function", "function": {"name": name}})),
            _ => None,
        };
        if let Some(mapped) = mapped {
            out.insert("tool_choice".into(), mapped);
        }
        if choice["disable_parallel_tool_use"].as_bool() == Some(true) {
            out.insert("parallel_tool_calls".into(), json!(false));
        }
    }
    if stream {
        out.insert("stream".into(), json!(true));
        // Without this OpenAI never reports usage on a stream, and `message_delta` would carry zeros.
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }
    let body = serde_json::to_vec(&Value::Object(out)).map_err(|e| e.to_string())?;
    Ok((body, RequestInfo { model, stream }))
}

/// A string, or the text of an array of blocks, joined by blank lines.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// An Anthropic user turn. Its `tool_result` blocks become `role: tool` messages, which OpenAI requires to
/// follow the assistant's `tool_calls` directly, so everything else in the turn goes into one user message
/// after them. Images inside a tool result can't ride on a tool message and move there too.
fn user_messages(content: &Value, out: &mut Vec<Value>) {
    let blocks = match content {
        Value::String(text) => return out.push(json!({"role": "user", "content": text})),
        Value::Array(blocks) => blocks,
        _ => return,
    };
    let mut parts = Vec::new();
    for block in blocks {
        if block["type"] != "tool_result" {
            parts.extend(user_part(block));
            continue;
        }
        let mut images = Vec::new();
        let mut text = match &block["content"] {
            Value::String(text) => text.clone(),
            Value::Array(inner) => {
                images.extend(inner.iter().filter(|b| b["type"] == "image").filter_map(user_part));
                text_of(&block["content"])
            }
            _ => String::new(),
        };
        if !images.is_empty() {
            text = if text.is_empty() {
                IMAGE_NOTE.to_string()
            } else {
                format!("{text}\n{IMAGE_NOTE}")
            };
        }
        out.push(json!({"role": "tool", "tool_call_id": block["tool_use_id"].as_str().unwrap_or_default(), "content": text}));
        parts.extend(images);
    }
    if !parts.is_empty() {
        out.push(json!({"role": "user", "content": parts}));
    }
}

fn user_part(block: &Value) -> Option<Value> {
    let source = &block["source"];
    match block["type"].as_str()? {
        "text" => block["text"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(|text| json!({"type": "text", "text": text})),
        "image" => {
            let url = match source["type"].as_str()? {
                "base64" => format!("data:{};base64,{}", source["media_type"].as_str()?, source["data"].as_str()?),
                "url" => source["url"].as_str()?.to_string(),
                _ => return None,
            };
            Some(json!({"type": "image_url", "image_url": {"url": url}}))
        }
        "document" => match source["type"].as_str()? {
            "base64" if source["media_type"] == "application/pdf" => Some(json!({
                "type": "file",
                "file": {"filename": "document.pdf", "file_data": format!("data:application/pdf;base64,{}", source["data"].as_str()?)},
            })),
            "text" => source["data"].as_str().map(|text| json!({"type": "text", "text": text})),
            _ => None,
        },
        _ => None,
    }
}

/// An Anthropic assistant turn: text blocks become the content, `tool_use` blocks become `tool_calls`.
/// Thinking blocks are dropped; their signatures mean nothing to another provider.
fn assistant_message(content: &Value, out: &mut Vec<Value>) {
    let mut calls = Vec::new();
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            for block in blocks.iter().filter(|b| b["type"] == "tool_use") {
                let input = if block["input"].is_null() {
                    json!({})
                } else {
                    block["input"].clone()
                };
                calls.push(json!({
                    "id": block["id"],
                    "type": "function",
                    "function": {"name": block["name"], "arguments": input.to_string()},
                }));
            }
            text_of(content)
        }
        _ => return,
    };
    if text.is_empty() && calls.is_empty() {
        return;
    }
    let mut message = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    out.push(message);
}

fn stop_reason(finish_reason: &str) -> &'static str {
    match finish_reason {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        // Claude Code shows a refusal to the user; an `end_turn` would pass for a finished answer.
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

/// OpenAI counts cached prompt tokens inside `prompt_tokens`; Anthropic reports them separately.
fn usage_of(usage: &Value) -> Usage {
    let prompt = usage["prompt_tokens"].as_u64().unwrap_or(0);
    let cached = usage["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0)
        .min(prompt);
    Usage {
        input_tokens: prompt - cached,
        output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: cached,
        cache_write_tokens: 0,
        thinking_tokens: usage["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
    }
}

/// A tool call's `arguments` string as the object Anthropic's `input` must be.
fn arguments(raw: &Value) -> Result<Value, String> {
    let raw = raw.as_str().unwrap_or_default().trim();
    if raw.is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(input) if input.is_object() => Ok(input),
        _ => Err(format!("provider returned tool arguments that are not a JSON object: {raw}")),
    }
}

/// A non-streaming Chat Completions response as an Anthropic message, with the usage it reported teed out
/// for the gateway's spend accounting.
pub fn translate_response(body: &[u8], info: &RequestInfo) -> Result<(Value, Usage), String> {
    let response: Value = serde_json::from_slice(body).map_err(|_| "provider response is not JSON".to_string())?;
    let choice = response["choices"].get(0).ok_or("provider response has no choices")?;
    let message = &choice["message"];
    let mut content = Vec::new();
    for text in [&message["content"], &message["refusal"]] {
        if let Some(text) = text.as_str().filter(|t| !t.is_empty()) {
            content.push(json!({"type": "text", "text": text}));
        }
    }
    for call in message["tool_calls"].as_array().into_iter().flatten() {
        content.push(json!({
            "type": "tool_use",
            "id": call["id"],
            "name": call["function"]["name"],
            "input": arguments(&call["function"]["arguments"])?,
        }));
    }
    let counted = usage_of(&response["usage"]);
    Ok((
        json!({
            "id": response["id"].as_str().unwrap_or("msg_openai"),
            "type": "message",
            "role": "assistant",
            "model": response["model"].as_str().unwrap_or(&info.model),
            "content": content,
            "stop_reason": stop_reason(choice["finish_reason"].as_str().unwrap_or_default()),
            "stop_sequence": null,
            "usage": counted.json(),
        }),
        counted,
    ))
}

/// An OpenAI error response as the status, Anthropic error type and message the gateway should answer
/// with. Claude Code decides whether to retry, and whether to compact, from these.
pub fn translate_error(status: StatusCode, body: &[u8], provider: &str) -> (StatusCode, &'static str, String) {
    let parsed: Value = serde_json::from_slice(body).unwrap_or_default();
    let error = &parsed["error"];
    let detail = error["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
    match error["code"].as_str().unwrap_or_default() {
        // Anthropic's wording: Claude Code compacts the conversation when it sees it.
        "context_length_exceeded" => {
            return (
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                format!("prompt is too long: {detail}"),
            );
        }
        // A 429 would be retried; an empty balance does not fill up with retries.
        "insufficient_quota" => {
            return (
                StatusCode::FORBIDDEN,
                "permission_error",
                format!("provider \"{provider}\": {detail}"),
            );
        }
        _ => {}
    }
    let kind = match status.as_u16() {
        400 | 422 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        503 | 529 => "overloaded_error",
        _ => "api_error",
    };
    (status, kind, format!("provider \"{provider}\": {detail}"))
}

fn event(out: &mut Vec<u8>, name: &str, data: Value) {
    out.extend_from_slice(format!("event: {name}\ndata: {data}\n\n").as_bytes());
}

/// Turns a Chat Completions SSE stream into the Anthropic event sequence, fed with upstream bytes as they
/// arrive. Every call returns whole events only (or nothing), so the gateway's keep-alive pings, which
/// may only land between events, stay legal.
///
/// Blocks are opened and closed one at a time, in the order Anthropic itself streams them. OpenAI
/// numbers parallel tool calls and streams each one's arguments before the next; a provider that
/// interleaves them gets an error event rather than silently mixed-up arguments.
pub struct StreamTranslator {
    model: String,
    line: Vec<u8>,
    data: String,
    started: bool,
    finished: bool,
    next_index: usize,
    text: Option<usize>,
    /// The open tool block: (OpenAI tool call index, Anthropic content block index).
    tool: Option<(u64, usize)>,
    seen_tools: HashSet<u64>,
    stop_reason: Option<&'static str>,
    usage: Usage,
}

impl StreamTranslator {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            line: Vec::new(),
            data: String::new(),
            started: false,
            finished: false,
            next_index: 0,
            text: None,
            tool: None,
            seen_tools: HashSet::new(),
            stop_reason: None,
            usage: Usage::default(),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// The stream's token totals, complete once it is finished.
    pub fn usage(&self) -> Usage {
        self.usage
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        self.line.extend_from_slice(chunk);
        // Lines are split on bytes, so a chunk boundary inside a UTF-8 character or a `data:` line just
        // waits for the rest of the line.
        while let Some(end) = self.line.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.line.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.handle_line(&line, &mut out);
            if self.finished {
                self.line.clear();
                return out;
            }
        }
        if self.line.len() > MAX_LINE {
            self.fail_into(&mut out, "provider sent a stream line over 8 MiB");
        }
        out
    }

    /// The upstream body ended. Without a finish reason the message is incomplete, which is an error.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        let rest = std::mem::take(&mut self.line);
        if !rest.is_empty() {
            self.handle_line(rest.strip_suffix(b"\r").unwrap_or(&rest), &mut out);
        }
        self.dispatch(&mut out);
        self.finish_into(&mut out);
        out
    }

    pub fn fail(&mut self, message: &str) -> Vec<u8> {
        let mut out = Vec::new();
        self.fail_into(&mut out, message);
        out
    }

    fn handle_line(&mut self, line: &[u8], out: &mut Vec<u8>) {
        if line.is_empty() {
            return self.dispatch(out);
        }
        // `event:`, `id:`, `retry:` and `:` comments (OpenRouter sends `: OPENROUTER PROCESSING`) carry
        // nothing the translation needs.
        if let Some(data) = line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(&String::from_utf8_lossy(data));
        }
    }

    fn dispatch(&mut self, out: &mut Vec<u8>) {
        let data = std::mem::take(&mut self.data);
        if data.is_empty() || self.finished {
            return;
        }
        if data == "[DONE]" {
            return self.finish_into(out);
        }
        let Ok(chunk) = serde_json::from_str::<Value>(&data) else {
            return self.fail_into(out, "provider sent a stream event that is not JSON");
        };
        // A stream that started with 200 can still end in an error, and by then there is no fallback:
        // Claude Code has to hear about it rather than wait on a half-built message.
        if chunk["error"].is_object() {
            let message = chunk["error"]["message"].as_str().unwrap_or("unknown error");
            return self.fail_into(out, &format!("provider stream failed: {message}"));
        }
        self.start(&chunk, out);
        if let Some(choice) = chunk["choices"].get(0) {
            let delta = &choice["delta"];
            for text in [&delta["content"], &delta["refusal"]] {
                if let Some(text) = text.as_str().filter(|t| !t.is_empty()) {
                    self.text_delta(text, out);
                }
            }
            for call in delta["tool_calls"].as_array().into_iter().flatten() {
                if !self.tool_delta(call, out) {
                    return;
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.stop_reason = Some(stop_reason(reason));
                self.close_blocks(out);
            }
        }
        if chunk["usage"].is_object() {
            self.usage = usage_of(&chunk["usage"]);
        }
    }

    fn start(&mut self, chunk: &Value, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        let message = json!({
            "id": chunk["id"].as_str().unwrap_or("msg_openai"),
            "type": "message",
            "role": "assistant",
            "model": chunk["model"].as_str().unwrap_or(&self.model),
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            // OpenAI only reports usage in the last chunk; the totals go out with `message_delta`.
            "usage": Usage::default().json(),
        });
        event(out, "message_start", json!({"type": "message_start", "message": message}));
    }

    fn open_block(&mut self, block: Value, out: &mut Vec<u8>) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        event(
            out,
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": block}),
        );
        index
    }

    fn text_delta(&mut self, text: &str, out: &mut Vec<u8>) {
        let index = match self.text {
            Some(index) => index,
            None => {
                self.close_blocks(out);
                let index = self.open_block(json!({"type": "text", "text": ""}), out);
                self.text = Some(index);
                index
            }
        };
        event(
            out,
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}),
        );
    }

    fn tool_delta(&mut self, call: &Value, out: &mut Vec<u8>) -> bool {
        let position = call["index"].as_u64().unwrap_or(0);
        let index = match self.tool {
            Some((open, index)) if open == position => index,
            _ if self.seen_tools.contains(&position) => {
                self.fail_into(
                    out,
                    "provider interleaved the arguments of parallel tool calls, which the openai wire does not support",
                );
                return false;
            }
            _ => {
                self.close_blocks(out);
                self.seen_tools.insert(position);
                let id = call["id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("call_{position}"));
                let block = json!({"type": "tool_use", "id": id, "name": call["function"]["name"].as_str().unwrap_or_default(), "input": {}});
                let index = self.open_block(block, out);
                self.tool = Some((position, index));
                index
            }
        };
        if let Some(partial) = call["function"]["arguments"].as_str().filter(|a| !a.is_empty()) {
            event(
                out,
                "content_block_delta",
                json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": partial}}),
            );
        }
        true
    }

    fn close_blocks(&mut self, out: &mut Vec<u8>) {
        for index in [self.text.take(), self.tool.take().map(|(_, index)| index)]
            .into_iter()
            .flatten()
        {
            event(
                out,
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            );
        }
    }

    fn finish_into(&mut self, out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        let Some(stop_reason) = self.stop_reason else {
            return self.fail_into(out, "provider ended the stream before finishing the message");
        };
        self.start(&Value::Null, out);
        self.close_blocks(out);
        let delta = json!({"type": "message_delta", "delta": {"stop_reason": stop_reason, "stop_sequence": null}, "usage": self.usage.json()});
        event(out, "message_delta", delta);
        event(out, "message_stop", json!({"type": "message_stop"}));
        self.finished = true;
    }

    fn fail_into(&mut self, out: &mut Vec<u8>, message: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        event(
            out,
            "error",
            json!({"type": "error", "error": {"type": "api_error", "message": message}}),
        );
    }
}

/// Wraps an upstream Chat Completions body so it streams Anthropic events. A chunk that translates to
/// nothing (the role-only first chunk, an empty delta, a comment) still yields an empty `Bytes`, which
/// `stream_body` counts as activity: a provider that keeps talking is not mistaken for a silent one.
/// `on_usage` runs once the translation is complete, with the usage the upstream reported, so the
/// gateway can price the response without reading these bytes a second time.
pub fn translate_stream(
    upstream: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    model: String,
    on_usage: impl FnOnce(Usage) + Send + 'static,
) -> impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static {
    let state = (Box::pin(upstream), StreamTranslator::new(model), false, Some(on_usage));
    futures_util::stream::unfold(state, |(mut upstream, mut translator, done, mut on_usage)| async move {
        if done {
            return None;
        }
        let (out, finished) = match upstream.next().await {
            Some(Ok(chunk)) => {
                let out = translator.push(&chunk);
                (out, translator.is_finished())
            }
            Some(Err(e)) => (translator.fail(&format!("provider stream failed: {}", e.without_url())), true),
            None => (translator.finish(), true),
        };
        // The usage is complete exactly when the translation is, so this is where it leaves the stream.
        if finished && let Some(on_usage) = on_usage.take() {
            on_usage(translator.usage());
        }
        Some((Ok(Bytes::from(out)), (upstream, translator, finished, on_usage)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(request: Value) -> Value {
        let (body, _) = translate_request(request.to_string().as_bytes()).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// `(event name, data)` for every event in an SSE body.
    fn events(bytes: &[u8]) -> Vec<(String, Value)> {
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(
            text.is_empty() || text.ends_with("\n\n"),
            "output must end on an event boundary: {text:?}"
        );
        text.split("\n\n")
            .filter(|e| !e.is_empty())
            .map(|e| {
                let name = e.lines().find_map(|l| l.strip_prefix("event: ")).unwrap().to_string();
                let data = serde_json::from_str(e.lines().find_map(|l| l.strip_prefix("data: ")).unwrap()).unwrap();
                (name, data)
            })
            .collect()
    }

    fn names(events: &[(String, Value)]) -> Vec<&str> {
        events.iter().map(|(name, _)| name.as_str()).collect()
    }

    fn sse(chunks: &[Value]) -> Vec<u8> {
        let mut out: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        out.push_str("data: [DONE]\n\n");
        out.into_bytes()
    }

    fn chunk(delta: Value, finish_reason: Value) -> Value {
        json!({"id": "chatcmpl-1", "model": "gpt-5.5", "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]})
    }

    #[test]
    fn only_the_messages_endpoint_is_translated() {
        assert_eq!(upstream_path("/v1/messages"), Some("/v1/chat/completions"));
        assert_eq!(upstream_path("/v1/messages/count_tokens"), None);
        assert_eq!(upstream_path("/v1/models"), None);
    }

    #[test]
    fn requests_keep_only_what_openai_accepts() {
        let out = translate(json!({
            "model": "gpt-5.5",
            "max_tokens": 32000,
            "stream": true,
            "system": [
                {"type": "text", "text": "You are Claude Code.", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "Be brief."},
            ],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}]}],
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "context_management": {"edits": []},
            "output_config": {"effort": "high"},
            "metadata": {"user_id": "user_abc"},
            "temperature": 1,
            "top_p": 0.9,
            "stop_sequences": ["</done>"],
        }));
        assert_eq!(
            out,
            json!({
                "model": "gpt-5.5",
                "messages": [
                    {"role": "system", "content": "You are Claude Code.\n\nBe brief."},
                    {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                ],
                "max_completion_tokens": 32000,
                "stream": true,
                "stream_options": {"include_usage": true},
            })
        );
        let unstreamed =
            translate(json!({"model": "gpt-5.5", "max_tokens": 10, "messages": [{"role": "user", "content": "hi"}]}));
        assert!(unstreamed.get("stream").is_none() && unstreamed.get("stream_options").is_none());
        assert_eq!(unstreamed["messages"], json!([{"role": "user", "content": "hi"}]));
    }

    #[test]
    fn tool_history_keeps_openai_message_order() {
        let out = translate(json!({
            "model": "gpt-5.5",
            "messages": [
                {"role": "user", "content": "list files"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "sig"},
                    {"type": "text", "text": "Looking."},
                    {"type": "tool_use", "id": "call_1", "name": "Bash", "input": {"command": "ls"}},
                    {"type": "tool_use", "id": "call_2", "name": "Read", "input": {"file_path": "a.png"}},
                ]},
                {"role": "user", "content": [
                    // Text *before* the tool results must still land after them.
                    {"type": "text", "text": "also this"},
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "a.png\n"},
                    {"type": "tool_result", "tool_use_id": "call_2", "content": [
                        {"type": "text", "text": "read"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBOR"}},
                    ]},
                ]},
                {"role": "assistant", "content": [{"type": "thinking", "thinking": "only thinking", "signature": "s"}]},
            ],
        }));
        assert_eq!(
            out["messages"],
            json!([
                {"role": "user", "content": "list files"},
                {"role": "assistant", "content": "Looking.", "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "Read", "arguments": "{\"file_path\":\"a.png\"}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "a.png\n"},
                {"role": "tool", "tool_call_id": "call_2", "content": "read\n(image output attached below)"},
                {"role": "user", "content": [
                    {"type": "text", "text": "also this"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBOR"}},
                ]},
            ])
        );
    }

    /// The shape Claude Code actually sent, captured from a real session: system messages sit inside
    /// `messages`, after the tool results, once as a string and once as blocks with `cache_control`.
    #[test]
    fn system_messages_inside_the_conversation_are_kept() {
        let out = translate(json!({
            "model": "gpt-5.5",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "run it"}]},
                {"role": "system", "content": "<system-reminder>environment</system-reminder>"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "call_a", "name": "Bash", "input": {"command": "true"}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_a", "content": "", "is_error": true}]},
                {"role": "system", "content": [{"type": "text", "text": "<system-reminder>budget</system-reminder>", "cache_control": {"type": "ephemeral"}}]},
            ],
        }));
        let roles: Vec<&str> = out["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "system", "assistant", "tool", "system"]);
        assert_eq!(
            out["messages"][4],
            json!({"role": "system", "content": "<system-reminder>budget</system-reminder>"})
        );
    }

    #[test]
    fn tools_and_tool_choice_map_to_functions() {
        let tools = json!([
            {"name": "Bash", "description": "Run a command", "input_schema": {"type": "object", "properties": {}}, "cache_control": {"type": "ephemeral"}},
            {"type": "web_search_20250305", "name": "web_search", "max_uses": 5},
        ]);
        let out = translate(json!({
            "model": "gpt-5.5", "messages": [{"role": "user", "content": "hi"}],
            "tools": tools, "tool_choice": {"type": "any", "disable_parallel_tool_use": true},
        }));
        assert_eq!(
            out["tools"],
            json!([{"type": "function", "function": {"name": "Bash", "description": "Run a command", "parameters": {"type": "object", "properties": {}}}}])
        );
        assert_eq!(out["tool_choice"], json!("required"));
        assert_eq!(out["parallel_tool_calls"], json!(false));

        let named =
            translate(json!({"model": "m", "messages": [], "tools": tools, "tool_choice": {"type": "tool", "name": "Bash"}}));
        assert_eq!(
            named["tool_choice"],
            json!({"type": "function", "function": {"name": "Bash"}})
        );

        // No usable tools means no tool_choice either: OpenAI rejects one without the other.
        let server_only = translate(json!({"model": "m", "messages": [], "tools": [tools[1]], "tool_choice": {"type": "auto"}}));
        assert!(server_only.get("tools").is_none() && server_only.get("tool_choice").is_none());
    }

    #[test]
    fn a_streamed_answer_becomes_the_anthropic_event_sequence() {
        let body = sse(&[
            chunk(json!({"role": "assistant", "content": ""}), Value::Null),
            chunk(json!({"content": "Hé"}), Value::Null),
            chunk(json!({"content": "llo"}), Value::Null),
            chunk(json!({}), json!("stop")),
            json!({"id": "chatcmpl-1", "model": "gpt-5.5", "choices": [], "usage": {"prompt_tokens": 120, "completion_tokens": 7, "prompt_tokens_details": {"cached_tokens": 100}}}),
        ]);
        let whole = StreamTranslator::new("gpt-5.5").push(&body);
        let out = events(&whole);
        assert_eq!(
            names(&out),
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        assert_eq!(out[0].1["message"]["id"], "chatcmpl-1");
        assert_eq!(out[1].1["content_block"], json!({"type": "text", "text": ""}));
        assert_eq!(out[2].1["delta"], json!({"type": "text_delta", "text": "Hé"}));
        assert_eq!(out[5].1["delta"]["stop_reason"], "end_turn");
        assert_eq!(
            out[5].1["usage"],
            json!({"input_tokens": 20, "output_tokens": 7, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 0})
        );

        // One byte at a time — splitting `data:` lines, the `\n\n` boundary and the two bytes of "é" —
        // produces the same events, and every push ends on an event boundary (checked by `events`).
        let mut translator = StreamTranslator::new("gpt-5.5");
        let mut bytewise = Vec::new();
        for byte in &body {
            let piece = translator.push(std::slice::from_ref(byte));
            events(&piece);
            bytewise.extend(piece);
        }
        assert!(translator.is_finished());
        assert_eq!(bytewise, whole);
    }

    /// The gateway prices a stream without re-parsing it: the usage the translator already extracted is
    /// handed over exactly once the translation completes, whatever the chunk layout.
    #[tokio::test]
    async fn a_finished_stream_hands_its_usage_to_the_gateway() {
        let body = sse(&[
            chunk(json!({"role": "assistant", "content": ""}), Value::Null),
            chunk(json!({}), json!("stop")),
            json!({"id": "c", "model": "gpt-5.5", "choices": [], "usage": {"prompt_tokens": 30, "completion_tokens": 4, "prompt_tokens_details": {"cached_tokens": 12}}}),
        ]);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = seen.clone();
        // A first chunk cut mid-line, so the tee cannot depend on chunk boundaries either.
        let (head, rest) = body.split_at(17);
        let chunks: Vec<reqwest::Result<Bytes>> = vec![Ok(Bytes::from(head.to_vec())), Ok(Bytes::from(rest.to_vec()))];
        let stream = translate_stream(futures_util::stream::iter(chunks), "gpt-5.5".into(), move |usage| {
            *sink.lock().unwrap() = Some(usage);
        });
        tokio::pin!(stream);
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.extend_from_slice(&item.expect("the stream translates"));
        }
        assert!(
            std::str::from_utf8(&out).unwrap().contains("event: message_stop"),
            "the stream translated"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            Some(Usage {
                input_tokens: 18,
                output_tokens: 4,
                cache_read_tokens: 12,
                cache_write_tokens: 0,
                thinking_tokens: 0
            })
        );
    }

    #[test]
    fn parallel_tool_calls_get_their_own_content_blocks() {
        let call = |index: u64, head: Option<(&str, &str)>, arguments: &str| {
            let mut call = json!({"index": index, "function": {"arguments": arguments}});
            if let Some((id, name)) = head {
                call["id"] = json!(id);
                call["type"] = json!("function");
                call["function"]["name"] = json!(name);
            }
            chunk(json!({"tool_calls": [call]}), Value::Null)
        };
        let out = events(&StreamTranslator::new("gpt-5.5").push(&sse(&[
            chunk(json!({"role": "assistant", "content": "Checking."}), Value::Null),
            call(0, Some(("call_a", "Bash")), ""),
            call(0, None, "{\"command\":"),
            call(0, None, "\"ls\"}"),
            call(1, Some(("call_b", "Read")), "{\"file_path\":\"a\"}"),
            chunk(json!({}), json!("tool_calls")),
        ])));
        let summary: Vec<String> = out
            .iter()
            .map(|(name, data)| match name.as_str() {
                "content_block_start" => format!("start {} {}", data["index"], data["content_block"]["type"]),
                "content_block_delta" => format!("delta {}", data["index"]),
                "content_block_stop" => format!("stop {}", data["index"]),
                other => other.to_string(),
            })
            .collect();
        assert_eq!(
            summary,
            [
                "message_start",
                "start 0 \"text\"",
                "delta 0",
                "stop 0",
                "start 1 \"tool_use\"",
                "delta 1",
                "delta 1",
                "stop 1",
                "start 2 \"tool_use\"",
                "delta 2",
                "stop 2",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(
            out[4].1["content_block"],
            json!({"type": "tool_use", "id": "call_a", "name": "Bash", "input": {}})
        );
        assert_eq!(
            out[5].1["delta"],
            json!({"type": "input_json_delta", "partial_json": "{\"command\":"})
        );
        assert_eq!(out[8].1["content_block"]["id"], "call_b");
        assert_eq!(out[11].1["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn a_mid_stream_error_ends_with_an_error_event() {
        let mut body = sse(&[chunk(json!({"role": "assistant", "content": "par"}), Value::Null)]);
        body.truncate(body.len() - "data: [DONE]\n\n".len());
        body.extend_from_slice(b"data: {\"error\":{\"message\":\"The server had an error\",\"type\":\"server_error\"}}\n\n");
        body.extend_from_slice(&sse(&[chunk(json!({"content": "tial"}), json!("stop"))]));

        let mut translator = StreamTranslator::new("gpt-5.5");
        let out = events(&translator.push(&body));
        assert_eq!(
            names(&out),
            ["message_start", "content_block_start", "content_block_delta", "error"]
        );
        assert_eq!(
            out[3].1["error"]["message"],
            "provider stream failed: The server had an error"
        );
        assert!(translator.is_finished());
        assert!(translator.finish().is_empty(), "nothing may follow the error event");
    }

    #[test]
    fn a_stream_that_ends_early_is_an_error_not_a_truncated_message() {
        let mut cut_off = StreamTranslator::new("gpt-5.5");
        cut_off.push(b"data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n");
        let out = events(&cut_off.finish());
        assert_eq!(names(&out), ["error"]);

        // A finished message whose upstream just forgot `[DONE]` (and its last blank line) is fine.
        let mut no_done = StreamTranslator::new("gpt-5.5");
        no_done.push(b"data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n");
        no_done.push(b"data: {\"id\":\"c\",\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}");
        let out = events(&no_done.finish());
        assert_eq!(names(&out), ["content_block_stop", "message_delta", "message_stop"]);
        assert_eq!(out[1].1["delta"]["stop_reason"], "max_tokens");
    }

    #[test]
    fn interleaved_tool_arguments_fail_loudly() {
        let call = |index: u64, arguments: &str| {
            chunk(
                json!({"tool_calls": [{"index": index, "id": format!("call_{index}"), "function": {"name": "T", "arguments": arguments}}]}),
                Value::Null,
            )
        };
        let out = events(&StreamTranslator::new("m").push(&sse(&[call(0, "{"), call(1, "{"), call(0, "}")])));
        assert_eq!(out.last().unwrap().0, "error");
        assert!(out.iter().all(|(name, _)| name != "message_stop"));
    }

    #[test]
    fn non_streaming_responses_translate_and_reject_bad_arguments() {
        let info = RequestInfo {
            model: "gpt-5.5".into(),
            stream: false,
        };
        let response = |arguments: &str| {
            json!({
                "id": "chatcmpl-2", "model": "gpt-5.5",
                "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {"role": "assistant", "content": "Done.", "tool_calls": [
                    {"id": "call_c", "type": "function", "function": {"name": "Bash", "arguments": arguments}},
                    {"id": "call_d", "type": "function", "function": {"name": "Noop", "arguments": ""}},
                ]}}],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5},
            })
            .to_string()
        };
        let (message, usage) = translate_response(response("{\"command\":\"pwd\"}").as_bytes(), &info).unwrap();
        assert_eq!(
            message,
            json!({
                "id": "chatcmpl-2", "type": "message", "role": "assistant", "model": "gpt-5.5",
                "content": [
                    {"type": "text", "text": "Done."},
                    {"type": "tool_use", "id": "call_c", "name": "Bash", "input": {"command": "pwd"}},
                    {"type": "tool_use", "id": "call_d", "name": "Noop", "input": {}},
                ],
                "stop_reason": "tool_use", "stop_sequence": null,
                "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
            })
        );
        assert_eq!(
            usage,
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
            "the usage is teed out for pricing"
        );
        assert!(translate_response(response("{\"command\":").as_bytes(), &info).is_err());
        assert!(translate_response(response("[1]").as_bytes(), &info).is_err());
        assert!(translate_response(b"<html>", &info).is_err());
    }

    #[test]
    fn errors_keep_their_meaning_for_claude_code() {
        let body = |code: &str, message: &str| {
            json!({"error": {"message": message, "type": "invalid_request_error", "code": code}}).to_string()
        };

        let (status, kind, message) = translate_error(
            StatusCode::BAD_REQUEST,
            body("context_length_exceeded", "maximum context length is 272000").as_bytes(),
            "openai",
        );
        assert_eq!((status, kind), (StatusCode::BAD_REQUEST, "invalid_request_error"));
        assert!(message.starts_with("prompt is too long"), "{message}");

        let (status, kind, _) = translate_error(
            StatusCode::TOO_MANY_REQUESTS,
            body("insufficient_quota", "out of credit").as_bytes(),
            "openai",
        );
        assert_eq!((status, kind), (StatusCode::FORBIDDEN, "permission_error"));

        let (status, kind, message) = translate_error(
            StatusCode::TOO_MANY_REQUESTS,
            body("rate_limit_exceeded", "slow down").as_bytes(),
            "openai",
        );
        assert_eq!(
            (status, kind, message.as_str()),
            (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                "provider \"openai\": slow down"
            )
        );

        assert_eq!(
            translate_error(StatusCode::UNAUTHORIZED, b"{}", "openai").1,
            "authentication_error"
        );
        let (status, kind, message) = translate_error(StatusCode::SERVICE_UNAVAILABLE, b"upstream connect error", "openai");
        assert_eq!(
            (status, kind, message.as_str()),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "overloaded_error",
                "provider \"openai\": HTTP 503"
            )
        );
    }
}
