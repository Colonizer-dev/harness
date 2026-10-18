use super::*;

/// Busy/in-flight counters, the provider's concurrency permit and the usage timer, held for as long
/// as the response body. The spend reservation is deliberately absent: it rides inside the usage
/// recorder instead, outliving the body until the cost it estimates has actually been recorded.
pub(super) type Guards = (Counted, Counted, Option<OwnedSemaphorePermit>, Timed);

/// Streams `chunks` downstream, translating provider errors and enforcing `timeout` as an overall
/// silence deadline. For an SSE response (`is_sse`), a `: keep-alive` comment — ignored by any
/// spec-compliant SSE parser — is sent every `SSE_PING_INTERVAL` of silence between events, so the
/// connection and any byte-level idle watchdog downstream see activity through a long prefill. A ping is
/// never sent inside a partly forwarded event, where it would corrupt a field or end the event early. A
/// ping doesn't reset the deadline: a provider that never produces a real byte still times out after `timeout`.
pub(super) fn stream_body(
    chunks: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    guards: Guards,
    timeout: Duration,
    is_sse: bool,
) -> impl futures_util::Stream<Item = std::io::Result<Bytes>> {
    let chunks = Box::pin(chunks);
    // tokio::time::Instant (not std::time::Instant) so this respects a paused clock under test.
    let start = (chunks, Some(guards), tokio::time::Instant::now(), true);
    futures_util::stream::unfold(start, move |(mut chunks, guards, silent_since, at_boundary)| async move {
        guards.as_ref()?;
        let remaining = timeout.saturating_sub(silent_since.elapsed());
        if remaining.is_zero() {
            return Some((
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "provider went silent")),
                (chunks, None, silent_since, at_boundary),
            ));
        }
        let can_ping = is_sse && at_boundary;
        let wait = if can_ping {
            remaining.min(SSE_PING_INTERVAL)
        } else {
            remaining
        };
        match tokio::time::timeout(wait, chunks.next()).await {
            Ok(Some(Ok(chunk))) => {
                let boundary = if chunk.is_empty() {
                    at_boundary
                } else {
                    ends_sse_event(&chunk)
                };
                Some((Ok(chunk), (chunks, guards, tokio::time::Instant::now(), boundary)))
            }
            Ok(Some(Err(e))) => Some((
                Err(std::io::Error::other(e.without_url())),
                (chunks, None, silent_since, at_boundary),
            )),
            Ok(None) => None,
            Err(_) if can_ping && wait < remaining => {
                Some((Ok(Bytes::from_static(SSE_PING)), (chunks, guards, silent_since, true)))
            }
            Err(_) => Some((
                Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "provider went silent")),
                (chunks, None, silent_since, at_boundary),
            )),
        }
    })
}

/// An SSE event ends with a blank line. An event boundary split across two chunks reads as "inside an
/// event", which only skips a ping, never corrupts the stream.
fn ends_sse_event(chunk: &[u8]) -> bool {
    chunk.ends_with(b"\n\n") || chunk.ends_with(b"\r\n\r\n") || chunk.ends_with(b"\r\r")
}

/// A body-end callback that records what passed through.
pub(super) type Recorder = Box<dyn FnOnce(Usage) + Send>;

/// Which API's usage spellings a response speaks: the Anthropic Messages usage object, or the two
/// OpenAI ones (`/v1/responses` and `/v1/chat/completions`). Decided by the route the request came
/// in on, never by sniffing the body.
#[derive(Clone, Copy, Default)]
pub(super) enum Shape {
    #[default]
    Anthropic,
    Openai,
}

/// What the gateway does with a request: pass the Anthropic body through untranslated, pass an
/// OpenAI-wire route's body through untranslated, or translate an Anthropic body into Chat
/// Completions. Chooses the response handler and the usage tap's [`Shape`] together, so a
/// passthrough response is never read as an Anthropic one.
pub(super) enum Routed {
    Anthropic,
    Openai,
    Translated(openai::RequestInfo),
}

/// Counts the tokens of a passing response without touching the bytes the colony receives:
/// [`counted_body`] feeds every forwarded chunk here and reads the totals when the body ends. Anything
/// unexpected (a proxy in front of the provider, a shape this version doesn't know) counts nothing and
/// never breaks the pass-through.
#[derive(Default)]
pub(super) enum UsageTap {
    /// A non-streaming JSON body, buffered up to `MAX_TAP_BODY` purely for counting.
    Json(Vec<u8>, Shape),
    /// An SSE body, read event by event as it passes.
    Sse(SseTap),
    /// A body too big or too odd to count. Bytes still pass; nothing is recorded.
    #[default]
    Skip,
}

impl UsageTap {
    pub(super) fn new(shape: Shape, is_sse: bool) -> Self {
        if is_sse {
            Self::Sse(SseTap::new(shape))
        } else {
            Self::Json(Vec::new(), shape)
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let overflow = matches!(self, Self::Json(buffer, _) if buffer.len() + chunk.len() > MAX_TAP_BODY);
        if overflow {
            *self = Self::Skip;
            return;
        }
        match self {
            Self::Json(buffer, _) => buffer.extend_from_slice(chunk),
            Self::Sse(tap) => tap.push(chunk),
            Self::Skip => {}
        }
    }

    fn finish(self) -> Usage {
        match self {
            // Malformed or truncated JSON parses to nothing, which is the deal: count only what is certain.
            Self::Json(buffer, shape) => serde_json::from_slice::<Value>(&buffer)
                .map(|body| shape.json_usage(&body["usage"]))
                .unwrap_or_default(),
            Self::Sse(tap) => tap.usage,
            Self::Skip => Usage::default(),
        }
    }
}

impl Shape {
    /// The usage object of a finished response, in this shape's spelling.
    fn json_usage(self, usage: &Value) -> Usage {
        match self {
            Self::Anthropic => anthropic_usage(usage),
            Self::Openai => openai_usage(usage),
        }
    }
}

/// The token counts an Anthropic usage object carries, as far as they are there. A missing or malformed
/// field counts as zero, so a half-readable body can only ever undercount.
pub(super) fn anthropic_usage(usage: &Value) -> Usage {
    Usage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
        cache_write_tokens: usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
        thinking_tokens: usage["output_tokens_details"]["thinking_tokens"].as_u64().unwrap_or(0),
    }
}

/// The token counts an OpenAI usage object carries, in either spelling: Chat Completions names the
/// sides `prompt_tokens`/`completion_tokens` ([`openai::usage_of`]), Responses `input_tokens`/
/// `output_tokens`. Cached input is a subset of the input total in both, so it comes back out of the
/// input side, like Anthropic's separately reported cache reads.
fn openai_usage(usage: &Value) -> Usage {
    if usage["prompt_tokens"].is_u64() {
        return openai::usage_of(usage);
    }
    let input = usage["input_tokens"].as_u64().unwrap_or(0);
    let cached = usage["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0)
        .min(input);
    Usage {
        input_tokens: input - cached,
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: cached,
        cache_write_tokens: 0,
        thinking_tokens: usage["output_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
    }
}

/// Assembles SSE events out of the chunks a forwarded body arrives in, keeping only what accounting
/// needs. Never holds the bytes back: it watches a private copy of the stream.
#[derive(Default)]
pub(super) struct SseTap {
    shape: Shape,
    line: Vec<u8>,
    data: String,
    usage: Usage,
}

impl SseTap {
    fn new(shape: Shape) -> Self {
        Self {
            shape,
            ..SseTap::default()
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.line.extend_from_slice(chunk);
        while let Some(end) = self.line.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.line.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.handle_line(&line);
        }
        // A line growing past the cap is not an event this accounting speaks; drop it and count nothing.
        if self.line.len() > MAX_TAP_BODY {
            self.line.clear();
            self.data.clear();
        }
    }

    fn handle_line(&mut self, line: &[u8]) {
        if line.is_empty() {
            return self.dispatch();
        }
        // `event:` names and `:` keep-alive comments (the gateway's own pings included) carry nothing to
        // count; the data's own `type` field does.
        if let Some(data) = line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(&String::from_utf8_lossy(data));
        }
    }

    fn dispatch(&mut self) {
        let data = std::mem::take(&mut self.data);
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        match self.shape {
            Shape::Anthropic => match event["type"].as_str() {
                Some("message_start") => {
                    let message = anthropic_usage(&event["message"]["usage"]);
                    self.usage.input_tokens = message.input_tokens;
                    self.usage.cache_read_tokens = message.cache_read_tokens;
                    self.usage.cache_write_tokens = message.cache_write_tokens;
                }
                // Deltas carry the running output and thinking totals, so the last one seen is the final count.
                Some("message_delta") => {
                    let output = event["usage"]["output_tokens"].as_u64().unwrap_or(0);
                    self.usage.output_tokens = self.usage.output_tokens.max(output);
                    let thinking = event["usage"]["output_tokens_details"]["thinking_tokens"]
                        .as_u64()
                        .unwrap_or(0);
                    self.usage.thinking_tokens = self.usage.thinking_tokens.max(thinking);
                }
                _ => {}
            },
            Shape::Openai => {
                // Responses reports usage once, on the terminal `response.completed` event's
                // `response.usage`; a chat completion just ends with a chunk whose `usage` is filled
                // in. Events without a usage object count nothing, so the last one carrying numbers
                // is the answer either way.
                let usage = match event["type"].as_str() {
                    Some("response.completed") => &event["response"]["usage"],
                    _ => &event["usage"],
                };
                if usage.is_object() {
                    self.usage = openai_usage(usage);
                }
            }
        }
    }
}

/// Wraps a body that is being forwarded to a colony, counting the tokens that pass through `tap` and
/// handing the totals to `record` once the body ends. `audit` rides along for the request's one
/// audit line: it counts the forwarded bytes and is finished — writing the line — when the body
/// ends, or when the stream holding it is dropped unread. The bytes themselves are never changed:
/// whatever the tap makes of the body, every chunk forwards exactly as it arrived.
pub(super) fn counted_body(
    inner: impl Stream<Item = std::io::Result<Bytes>> + Send + 'static,
    tap: UsageTap,
    record: Option<Recorder>,
    audit: Option<GatewayAudit>,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    let state = (Box::pin(inner), tap, record, audit);
    futures_util::stream::unfold(state, |(mut inner, mut tap, record, audit)| async move {
        let item = match inner.next().await {
            Some(Ok(chunk)) => {
                tap.push(&chunk);
                if let Some(audit) = &audit {
                    audit.add_bytes(chunk.len());
                }
                Some(Ok(chunk))
            }
            // An error mid-body is a response the colony never got whole; the audit says so when
            // the line lands. The error itself forwards exactly as it arrived.
            Some(Err(e)) => {
                if let Some(audit) = &audit {
                    audit.note_body_error();
                }
                Some(Err(e))
            }
            None => None,
        };
        let Some(item) = item else {
            // The body is over (or its error was delivered): report whatever the tap managed to read.
            let usage = tap.finish();
            if let Some(audit) = &audit {
                audit.finish(usage);
            }
            if usage.total_tokens() > 0
                && let Some(record) = record
            {
                record(usage);
            }
            return None;
        };
        Some((item, (inner, tap, record, audit)))
    })
}

/// The body-end callback for a routed response: add its spend to the colony and re-check its budget.
/// That runs as its own task, so accounting never delays the colony's bytes. The task owns the
/// request's [`Reserved`] estimate and gives it back only once the real cost has landed, so no
/// window is left where a new request could see neither the reservation nor the recorded spend.
/// A recorder that is dropped without ever running (an uncounted body, an abandoned stream) still
/// drops the reservation it carries, at that moment.
pub(super) fn usage_recorder(
    app: &Shared,
    colony: &str,
    provider: &Provider,
    model: Option<&str>,
    reservation: Reserved,
) -> Recorder {
    let (app, colony, provider, model) = (app.clone(), colony.to_string(), provider.clone(), model.map(str::to_string));
    Box::new(move |usage| {
        tokio::spawn(async move {
            // The estimate goes back in the same critical section that adds the real cost, so no
            // reader ever counts both (or neither). If the cost is never recorded (zero, or the
            // colony is gone) the closure is dropped unrun and the guard goes with it.
            crate::lifecycle::record_routed_usage(&app, &colony, &provider, model.as_deref(), usage, move || drop(reservation))
                .await;
        });
    })
}

/// `{base_url}{rest}?{query}`, where `rest` is the request path after `/providers/{id}`. The path
/// is held to plain URL characters with no `.`/`..` segment, and the query to the same kind of set
/// plus `=&` — `None` for anything else, before the request is built. A base_url whose path already
/// ends in a version segment is the API root as its provider documents it — `https://api.x.ai/v1`,
/// BytePlus's `…/api/coding/v3`, Volcengine's `…/api/v3` — so a request path that starts with `/v1/`
/// drops that `/v1` instead of doubling the version (issue #1018). Every route joins here: the
/// anthropic wire, the openai passthrough, the translated chat completion and the `/v1/models` probe.
pub(super) fn upstream_url(base_url: &str, rest: &str, query: Option<&str>) -> Option<String> {
    let clean = rest.starts_with('/')
        && rest.chars().all(|c| c.is_ascii_alphanumeric() || "/_-.".contains(c))
        && !rest.split('/').any(|segment| segment == "." || segment == "..")
        && query.is_none_or(|q| q.chars().all(|c| c.is_ascii_alphanumeric() || "/_-=&".contains(c)));
    if !clean {
        return None;
    }
    let base = base_url.trim_end_matches('/');
    let rest = match ends_in_version_segment(base) && rest.starts_with("/v1/") {
        true => &rest["/v1".len()..],
        false => rest,
    };
    let query = query.map(|q| format!("?{q}")).unwrap_or_default();
    Some(format!("{base}{rest}{query}"))
}

/// Where a request went, safe to log and to show the operator: scheme, host, port and path. The
/// userinfo, query and fragment are dropped, so no credential a base URL or query might carry leaks.
pub(super) fn redacted_url(url: &reqwest::Url) -> String {
    let mut shown = url.clone();
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    shown.set_query(None);
    shown.set_fragment(None);
    shown.to_string()
}

/// The words a 404/405 from upstream adds: those statuses mean the provider has no such route, which
/// is almost always a base URL that does not match the provider's documented API root (issue #1018).
pub(super) fn wrong_route_hint(status: u16, url: &str) -> Option<String> {
    matches!(status, 404 | 405).then(|| format!("upstream answered {status} at {url}; check the provider's base URL"))
}

/// Whether the last path segment of `base` (no trailing `/`) is `v<digits>`: `/v1`, `/v3`,
/// `/api/coding/v3`. Only the path counts, so a host that happens to be named `v1` does not.
fn ends_in_version_segment(base: &str) -> bool {
    let authority_and_path = base.split_once("://").map_or(base, |(_, rest)| rest);
    let Some((_, path)) = authority_and_path.split_once('/') else {
        return false;
    };
    let last = path.rsplit('/').next().unwrap_or_default();
    last.strip_prefix('v')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// The OpenAI-wire routes the gateway forwards untranslated to an `openai`-wire provider (issue
/// #629): what a runner speaking that wire itself posts — codex the Responses API, grok-build Chat
/// Completions.
pub(super) fn is_openai_passthrough(rest: &str) -> bool {
    matches!(rest, "/v1/responses" | "/v1/chat/completions")
}

/// The body a passthrough route forwards: the connection policy applies exactly as it does to
/// `/v1/messages` (#295) — its `model_map` renames the model, and `disabled_tools` matches a
/// top-level `tools[].name` that OpenAI-shaped tool entries (nested under `function`) never carry —
/// and a streaming chat completion gets `stream_options.include_usage` unless the colony asked for
/// usage itself, because without it the final usage chunk never comes and accounting counts nothing.
/// Every other field forwards untouched; `stream_options` is chat-only, Responses reports usage on
/// its terminal `response.completed` event regardless. With neither rewrite pending the colony's own
/// bytes go out unchanged, never re-serialized — the same byte-identical escape hatch the policy has.
pub(super) fn openai_passthrough_body(rest: &str, body: &[u8], provider: &Provider) -> Result<Vec<u8>, String> {
    let policy = apply_connection_policy(body, provider);
    let body = policy.as_deref().unwrap_or(body);
    // The body's one parse: it decides whether either rewrite applies and is the rewrite's input.
    let mut request: Value = serde_json::from_slice(body).map_err(|e| format!("request body is not JSON: {e}"))?;
    let wants_usage = rest == "/v1/chat/completions"
        && request.get("stream").and_then(Value::as_bool) == Some(true)
        && request
            .get("stream_options")
            .and_then(|options| options.get("include_usage"))
            .and_then(Value::as_bool)
            != Some(true);
    if policy.is_none() && !wants_usage {
        return Ok(body.to_vec());
    }
    let object = request.as_object_mut().ok_or("request body is not a JSON object")?;
    if wants_usage {
        match object.get_mut("stream_options") {
            Some(options @ Value::Object(_)) => {
                options["include_usage"] = json!(true);
            }
            _ => {
                object.insert("stream_options".into(), json!({"include_usage": true}));
            }
        }
    }
    serde_json::to_vec(&request).map_err(|e| e.to_string())
}

/// The provider's credential header, if it has one.
pub fn credential_header(app: &App, provider: &Provider) -> Option<(HeaderName, HeaderValue)> {
    let key = app.provider_key(&provider.id)?;
    let (name, value) = match provider.auth.as_str() {
        "x-api-key" => (HeaderName::from_static("x-api-key"), key),
        "bearer" => (HeaderName::from_static("authorization"), format!("Bearer {key}")),
        _ => return None,
    };
    let mut value = HeaderValue::from_str(&value).ok()?;
    value.set_sensitive(true);
    Some((name, value))
}

/// Only what an Anthropic-compatible endpoint needs; the colony's own credentials never pass through.
pub(super) fn forward_headers(incoming: &HeaderMap, credential: Option<(HeaderName, HeaderValue)>) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in FORWARD_HEADERS {
        if let Some(value) = incoming.get(name) {
            out.insert(name, value.clone());
        }
    }
    if let Some(betas) = incoming.get("anthropic-beta").and_then(|v| v.to_str().ok()) {
        let betas = strip_oauth_betas(betas);
        if let Ok(value) = HeaderValue::from_str(&betas)
            && !betas.is_empty()
        {
            out.insert("anthropic-beta", value);
        }
    }
    if let Some((name, value)) = credential {
        out.insert(name, value);
    }
    out
}
