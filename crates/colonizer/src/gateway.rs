//! Provider gateway (docs/protocol.md §6.5). Colonies send routed model requests here instead of
//! straight to the provider: the mothership is on the operator's networks (tailnet, LAN), holds the
//! provider keys, and sees every colony, so it can queue requests per provider, apply long timeouts,
//! and report which colonies are waiting on a model. Colonies authenticate with a per-colony token.

use crate::{
    ApiResult, App, Shared, client_error, openai,
    providers::{Provider, Usage, Wire, strip_oauth_betas},
    util::read_trimmed,
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::{DateTime, Utc};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const COLONY_HEADER: &str = "x-colonizer-colony";
pub const FALLBACK_HEADER: &str = "x-colonizer-fallback";
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;
/// Large contexts with images can exceed axum's 2 MB default.
const MAX_BODY: usize = 64 * 1024 * 1024;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const FORWARD_HEADERS: [&str; 3] = ["content-type", "accept", "anthropic-version"];
const DROP_RESPONSE_HEADERS: [&str; 6] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "content-length",
];
/// How long an SSE response may go silent before the gateway sends a comment line to keep the
/// connection (and any byte-level idle watchdog downstream) alive during a long prefill.
const SSE_PING_INTERVAL: Duration = Duration::from_secs(15);
const SSE_PING: &[u8] = b": keep-alive\n\n";
/// Longest response body buffered purely to count a non-streaming usage. Past this the response still
/// forwards whole; its tokens just aren't priced.
const MAX_TAP_BODY: usize = 4 * 1024 * 1024;

/// How long dirty usage counters may go unwritten; a crash loses at most this much of the tally.
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Cumulative per-provider usage, kept across restarts in `<data_dir>/provider-usage.json`. The live
/// `in_flight`/`queued` gauges are zero whenever nobody is mid-request, so these counters are what says
/// whether a request has ever actually gone to the provider.
///
/// - `requests`: requests the gateway accepted for this provider. Counted once everything that can refuse a
///   request locally has passed (colony auth, provider lookup, path and body translation), so queueing, the
///   upstream call and the streamed body are all included, and a request the gateway itself refuses is not.
///   An attempt that queues past `queue_timeout_secs` and never reaches the provider still counts — it was a
///   real request against this provider — so `requests` is not a count of requests the provider saw.
/// - `failures`: requests that produced no usable upstream response — one of the gateway's three fallback
///   answers (queue timeout, unreachable, timeout), an upstream status >= 400, or an openai-wire response
///   whose body failed or never finished. A failure after the headers, part-way through a streamed body,
///   is not counted. A subset of `requests`.
/// - `fallbacks`: requests that will fall back to Claude. A prediction, not an observation: the gateway
///   answered 502/503/504 with `x-colonizer-fallback` and the provider has a `fallback_model`, which is
///   exactly when the colony's model router (router.mjs) retries on Claude. The retry itself never comes
///   back through the gateway. A subset of `failures`.
/// - `duration_ms`: cumulative wall-clock time of dispatched requests, including streaming the response
///   body. Timed from when a request's concurrency slot was acquired, so time spent queued is never counted.
/// - `last_request_at`: when the last request was accepted (before any queue wait), RFC3339 like the other
///   timestamps here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderUsage {
    pub requests: u64,
    pub failures: u64,
    pub fallbacks: u64,
    pub duration_ms: u64,
    pub last_request_at: Option<DateTime<Utc>>,
}

/// Counts up while alive; used for in-flight and queued requests.
struct Counted(Arc<AtomicU64>);

impl Counted {
    fn new(counter: &Arc<AtomicU64>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter.clone())
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Adds the elapsed wall-clock time to the provider's cumulative `duration_ms` when dropped — the same
/// lifetime as the request's other guards, so the streamed body is included. Created once the request's
/// concurrency slot is acquired, not while it waits for one, so `duration_ms` measures dispatched time only.
struct Timed {
    counters: Arc<UsageCounters>,
    start: Instant,
}

impl Timed {
    fn new(counters: Arc<UsageCounters>) -> Self {
        Self {
            counters,
            start: Instant::now(),
        }
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        self.counters
            .duration_ms
            .fetch_add(self.start.elapsed().as_millis() as u64, Ordering::SeqCst);
        self.counters.dirty.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct ProviderStats {
    in_flight: Arc<AtomicU64>,
    queued: Arc<AtomicU64>,
}

/// Live cumulative usage for one provider, seeded from disk at startup and written back by
/// `flush_usage` when dirty. The request path only touches these atomics, never the file.
#[derive(Default)]
struct UsageCounters {
    requests: AtomicU64,
    failures: AtomicU64,
    fallbacks: AtomicU64,
    duration_ms: AtomicU64,
    last_request_at: Mutex<Option<DateTime<Utc>>>,
    /// Set by every change; `flush_usage` clears it and writes.
    dirty: AtomicBool,
}

impl UsageCounters {
    fn seeded(usage: ProviderUsage) -> Self {
        Self {
            requests: AtomicU64::new(usage.requests),
            failures: AtomicU64::new(usage.failures),
            fallbacks: AtomicU64::new(usage.fallbacks),
            duration_ms: AtomicU64::new(usage.duration_ms),
            last_request_at: Mutex::new(usage.last_request_at),
            dirty: AtomicBool::new(false),
        }
    }

    fn add_request(&self) {
        self.requests.fetch_add(1, Ordering::SeqCst);
        *self.last_request_at.lock().unwrap() = Some(Utc::now());
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// A failure with no fallback answer: an upstream status >= 400, or an openai-wire body that failed or
    /// never finished. The colony's router retries none of these — only the gateway's own three fallback
    /// errors get that.
    fn add_failure(&self) {
        self.failures.fetch_add(1, Ordering::SeqCst);
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// One of the three gateway-level fallback errors, and — when the provider has a fallback model —
    /// the fallback to Claude the colony's router will make with it (see [`ProviderUsage::fallbacks`]).
    fn add_failure_with_fallback(&self, provider: &Provider) {
        self.add_failure();
        if provider.fallback_model.as_deref().is_some_and(|m| !m.is_empty()) {
            self.fallbacks.fetch_add(1, Ordering::SeqCst);
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    fn snapshot(&self) -> ProviderUsage {
        ProviderUsage {
            requests: self.requests.load(Ordering::SeqCst),
            failures: self.failures.load(Ordering::SeqCst),
            fallbacks: self.fallbacks.load(Ordering::SeqCst),
            duration_ms: self.duration_ms.load(Ordering::SeqCst),
            last_request_at: *self.last_request_at.lock().unwrap(),
        }
    }
}

struct Limit {
    max: u64,
    slots: Arc<Semaphore>,
}

pub struct Gateway {
    client: reqwest::Client,
    stats: Mutex<HashMap<String, Arc<ProviderStats>>>,
    limits: Mutex<HashMap<String, Limit>>,
    /// Requests each colony has open through the gateway, queued or streaming.
    colonies: Mutex<HashMap<String, Arc<AtomicU64>>>,
    /// Cumulative usage per provider, seeded from `usage_file` at startup and written back to it when dirty.
    usage: Mutex<HashMap<String, Arc<UsageCounters>>>,
    usage_file: PathBuf,
}

impl Gateway {
    pub fn new(data_dir: &std::path::Path) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let usage_file = data_dir.join("provider-usage.json");
        // A missing or corrupt file means the counters start over, never that the gateway fails.
        let saved: BTreeMap<String, ProviderUsage> = std::fs::read(&usage_file)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        let usage: HashMap<String, Arc<UsageCounters>> = saved
            .into_iter()
            .map(|(id, usage)| (id, Arc::new(UsageCounters::seeded(usage))))
            .collect();
        Ok(Self {
            client,
            stats: Default::default(),
            limits: Default::default(),
            colonies: Default::default(),
            usage: Mutex::new(usage),
            usage_file,
        })
    }

    fn stats(&self, provider: &str) -> Arc<ProviderStats> {
        self.stats.lock().unwrap().entry(provider.to_string()).or_default().clone()
    }

    /// `(in_flight, queued)` for a provider across all colonies.
    pub fn load(&self, provider: &str) -> (u64, u64) {
        let stats = self.stats(provider);
        (stats.in_flight.load(Ordering::SeqCst), stats.queued.load(Ordering::SeqCst))
    }

    fn usage_counters(&self, provider: &str) -> Arc<UsageCounters> {
        self.usage.lock().unwrap().entry(provider.to_string()).or_default().clone()
    }

    /// Cumulative usage for a provider since the counters were first kept.
    pub fn usage(&self, provider: &str) -> ProviderUsage {
        self.usage_counters(provider).snapshot()
    }

    /// Writes the whole usage map atomically (tmp + rename). Unlike the crate's other JSON state this file
    /// has three concurrent writers — the flush loop, the shutdown flush and [`Self::forget_usage`] — so the
    /// tmp path is unique per call: writers sharing one path interleave their writes and can rename a
    /// half-overwritten file into place, which `Gateway::new` would read as corrupt and silently reset every
    /// provider's tally. Renames can still land out of order, but each one is a complete snapshot, so the
    /// worst a lost race does is persist a slightly stale tally until the next flush.
    fn write_usage(&self) {
        let snapshot: BTreeMap<String, ProviderUsage> = self
            .usage
            .lock()
            .unwrap()
            .iter()
            .map(|(id, counters)| (id.clone(), counters.snapshot()))
            .collect();
        if let Ok(data) = serde_json::to_vec_pretty(&snapshot) {
            let tmp = self.usage_file.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
            if std::fs::write(&tmp, data).is_ok() {
                let _ = std::fs::rename(&tmp, &self.usage_file);
            } else {
                // A failed write may have left a partial tmp behind; it must not pile up in the data dir.
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// Writes the usage counters if anything changed since the last flush. Called every
    /// [`USAGE_FLUSH_INTERVAL`] and on shutdown, never per request: the request path only touches the
    /// in-memory counters, so a crash loses at most [`USAGE_FLUSH_INTERVAL`] of the tally.
    pub fn flush_usage(&self) {
        // Swaps every flag: `any` would stop at the first dirty entry and leave the rest set even though
        // write_usage below persists the whole map.
        let mut dirty = false;
        {
            let map = self.usage.lock().unwrap();
            for counters in map.values() {
                dirty |= counters.dirty.swap(false, Ordering::SeqCst);
            }
        }
        if !dirty {
            return;
        }
        if let Some(dir) = self.usage_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        self.write_usage();
    }

    /// Forgets a provider's usage and flushes at once, so deleting the provider also deletes its tally
    /// even if nothing else is dirty.
    pub fn forget_usage(&self, provider: &str) {
        if self.usage.lock().unwrap().remove(provider).is_some() {
            if let Some(dir) = self.usage_file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            self.write_usage();
        }
    }

    fn colony_counter(&self, colony: &str) -> Arc<AtomicU64> {
        self.colonies.lock().unwrap().entry(colony.to_string()).or_default().clone()
    }

    /// True while the colony is waiting on a model through the gateway; the watchdog counts that as progress.
    pub fn colony_busy(&self, colony: &str) -> bool {
        self.colonies
            .lock()
            .unwrap()
            .get(colony)
            .is_some_and(|c| c.load(Ordering::SeqCst) > 0)
    }

    /// The provider's request slots, or `None` when it has no concurrency limit. A changed limit gets a
    /// fresh semaphore; requests holding the old one finish without counting against the new limit.
    fn slots(&self, provider: &str, max: Option<u64>) -> Option<Arc<Semaphore>> {
        let mut limits = self.limits.lock().unwrap();
        let Some(max) = max else {
            limits.remove(provider);
            return None;
        };
        let limit = limits.entry(provider.to_string()).or_insert_with(|| Limit {
            max,
            slots: Arc::new(Semaphore::new(max as usize)),
        });
        if limit.max != max {
            *limit = Limit {
                max,
                slots: Arc::new(Semaphore::new(max as usize)),
            };
        }
        Some(limit.slots.clone())
    }
}

impl App {
    pub fn gateway_token_file(&self, session: &str) -> std::path::PathBuf {
        self.session_dir(session).join("gateway-token")
    }

    /// The live colony a gateway token belongs to.
    async fn colony_for_token(&self, token: &str) -> Option<String> {
        if token.len() < 32 {
            return None;
        }
        let sessions = self.sessions.read().await;
        sessions
            .iter()
            .filter(|s| s.status.is_live())
            .find(|s| {
                read_trimmed(&self.gateway_token_file(&s.id)).is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes()))
            })
            .map(|s| s.id.clone())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/providers/{id}/{*path}", any(proxy))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}

/// Flushes dirty usage counters every [`USAGE_FLUSH_INTERVAL`]; spawned once at startup.
pub async fn flush_loop(app: Shared) {
    loop {
        tokio::time::sleep(USAGE_FLUSH_INTERVAL).await;
        app.gateway.flush_usage();
    }
}

/// An error in Anthropic's shape, so Claude Code reports it like any API error.
fn api_error(status: StatusCode, kind: &str, message: impl Into<String>, fallback: Option<&'static str>) -> Response {
    let mut response = (
        status,
        Json(json!({"type": "error", "error": {"type": kind, "message": message.into()}})),
    )
        .into_response();
    if let Some(reason) = fallback {
        response
            .headers_mut()
            .insert(FALLBACK_HEADER, HeaderValue::from_static(reason));
    }
    response
}

/// Busy/in-flight counters, the provider's concurrency permit and the usage timer, held for as long as
/// the response body.
type Guards = (Counted, Counted, Option<OwnedSemaphorePermit>, Timed);

/// Streams `chunks` downstream, translating provider errors and enforcing `timeout` as an overall
/// silence deadline. For an SSE response (`is_sse`), a `: keep-alive` comment — ignored by any
/// spec-compliant SSE parser — is sent every `SSE_PING_INTERVAL` of silence between events, so the
/// connection and any byte-level idle watchdog downstream see activity through a long prefill. A ping is
/// never sent inside a partly forwarded event, where it would corrupt a field or end the event early. A
/// ping doesn't reset the deadline: a provider that never produces a real byte still times out after `timeout`.
fn stream_body(
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
type Recorder = Box<dyn FnOnce(Usage) + Send>;

/// Counts the tokens of a passing Anthropic response without touching the bytes the colony receives:
/// [`counted_body`] feeds every forwarded chunk here and reads the totals when the body ends. Anything
/// unexpected (a proxy in front of the provider, a shape this version doesn't know) counts nothing and
/// never breaks the pass-through.
#[derive(Default)]
enum UsageTap {
    /// A non-streaming JSON body, buffered up to `MAX_TAP_BODY` purely for counting.
    Json(Vec<u8>),
    /// An SSE body, read event by event as it passes: `message_start` fixes the input side,
    /// `message_delta` carries the running output total.
    Sse(SseTap),
    /// A body too big or too odd to count. Bytes still pass; nothing is recorded.
    #[default]
    Skip,
}

impl UsageTap {
    fn anthropic(is_sse: bool) -> Self {
        if is_sse {
            Self::Sse(SseTap::default())
        } else {
            Self::Json(Vec::new())
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let overflow = matches!(self, Self::Json(buffer) if buffer.len() + chunk.len() > MAX_TAP_BODY);
        if overflow {
            *self = Self::Skip;
            return;
        }
        match self {
            Self::Json(buffer) => buffer.extend_from_slice(chunk),
            Self::Sse(tap) => tap.push(chunk),
            Self::Skip => {}
        }
    }

    fn finish(self) -> Usage {
        match self {
            // Malformed or truncated JSON parses to nothing, which is the deal: count only what is certain.
            Self::Json(buffer) => serde_json::from_slice::<Value>(&buffer)
                .map(|body| anthropic_usage(&body["usage"]))
                .unwrap_or_default(),
            Self::Sse(tap) => tap.usage,
            Self::Skip => Usage::default(),
        }
    }
}

/// The token counts an Anthropic usage object carries, as far as they are there. A missing or malformed
/// field counts as zero, so a half-readable body can only ever undercount.
fn anthropic_usage(usage: &Value) -> Usage {
    Usage {
        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
        cache_read_tokens: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
        cache_write_tokens: usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
    }
}

/// Assembles SSE events out of the chunks a forwarded body arrives in, keeping only what accounting
/// needs. Never holds the bytes back: it watches a private copy of the stream.
#[derive(Default)]
struct SseTap {
    line: Vec<u8>,
    data: String,
    usage: Usage,
}

impl SseTap {
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
        match event["type"].as_str() {
            Some("message_start") => {
                let message = anthropic_usage(&event["message"]["usage"]);
                self.usage.input_tokens = message.input_tokens;
                self.usage.cache_read_tokens = message.cache_read_tokens;
                self.usage.cache_write_tokens = message.cache_write_tokens;
            }
            // Deltas carry the running output total, so the last one seen is the final count.
            Some("message_delta") => {
                let output = event["usage"]["output_tokens"].as_u64().unwrap_or(0);
                self.usage.output_tokens = self.usage.output_tokens.max(output);
            }
            _ => {}
        }
    }
}

/// Wraps a body that is being forwarded to a colony, counting the tokens that pass through `tap` and
/// handing the totals to `record` once the body ends. The bytes themselves are never changed: whatever
/// the tap makes of the body, every chunk forwards exactly as it arrived.
fn counted_body(
    inner: impl Stream<Item = std::io::Result<Bytes>> + Send + 'static,
    tap: UsageTap,
    record: Option<Recorder>,
) -> impl Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    let state = (Box::pin(inner), tap, record);
    futures_util::stream::unfold(state, |(mut inner, mut tap, record)| async move {
        let item = match inner.next().await {
            Some(Ok(chunk)) => {
                tap.push(&chunk);
                Some(Ok(chunk))
            }
            other => other,
        };
        let Some(item) = item else {
            // The body is over (or its error was delivered): report whatever the tap managed to read.
            let usage = tap.finish();
            if usage.total_tokens() > 0
                && let Some(record) = record
            {
                record(usage);
            }
            return None;
        };
        Some((item, (inner, tap, record)))
    })
}

/// The body-end callback for a routed response: add its spend to the colony and re-check its budget.
/// That runs as its own task, so accounting never delays the colony's bytes.
fn usage_recorder(app: &Shared, colony: &str, provider: &Provider) -> Recorder {
    let (app, colony, provider) = (app.clone(), colony.to_string(), provider.clone());
    Box::new(move |usage| {
        tokio::spawn(async move { crate::lifecycle::record_routed_usage(&app, &colony, &provider, usage).await });
    })
}

/// `{base_url}{rest}?{query}`, where `rest` is the request path after `/providers/{id}`.
fn upstream_url(base_url: &str, rest: &str, query: Option<&str>) -> Option<String> {
    let clean = rest.starts_with('/')
        && rest.chars().all(|c| c.is_ascii_alphanumeric() || "/_-.".contains(c))
        && !rest.split('/').any(|segment| segment == "." || segment == "..");
    if !clean {
        return None;
    }
    let query = query.map(|q| format!("?{q}")).unwrap_or_default();
    Some(format!("{}{rest}{query}", base_url.trim_end_matches('/')))
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
fn forward_headers(incoming: &HeaderMap, credential: Option<(HeaderName, HeaderValue)>) -> HeaderMap {
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

async fn proxy(
    State(app): State<Shared>,
    Path((id, _)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = headers.get(COLONY_HEADER).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let Some(colony) = app.colony_for_token(token).await else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "colonizer gateway: unknown colony",
            None,
        );
    };
    let Some(provider) = app.providers().into_iter().find(|p| p.id == id) else {
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("colonizer gateway: no provider \"{id}\""),
            None,
        );
    };
    // Refused before it waits for a slot, and the colony is stopped like the max-duration path stops one.
    // The 403 follows the empty-balance precedent in openai.rs: Claude Code does not retry it in a loop.
    if crate::lifecycle::enforce_budget(&app, &colony).await {
        return api_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("colonizer gateway: colony {colony} passed its spend budget and was stopped; raise the budget and resume it"),
            None,
        );
    }
    let rest = uri.path().strip_prefix(&format!("/providers/{id}")).unwrap_or_default();
    // Everything that can refuse the request happens here, before it waits for a slot. The anthropic wire
    // never parses the body; the openai wire has to rebuild it.
    let (url, upstream_headers, body, translation) = match provider.wire {
        Wire::Anthropic => {
            let Some(url) = upstream_url(&provider.base_url, rest, uri.query()) else {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    "colonizer gateway: unsupported path",
                    None,
                );
            };
            (url, forward_headers(&headers, credential_header(&app, &provider)), body, None)
        }
        Wire::Openai => {
            // 404 is also what tells the colony router to estimate `count_tokens` itself.
            let Some(path) = openai::upstream_path(rest) else {
                return api_error(
                    StatusCode::NOT_FOUND,
                    "not_found_error",
                    format!("colonizer gateway: provider \"{id}\" does not serve {rest}"),
                    None,
                );
            };
            let (body, info) = match openai::translate_request(&body) {
                Ok(translated) => translated,
                Err(message) => {
                    return api_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_request_error",
                        format!("colonizer gateway: {message}"),
                        None,
                    );
                }
            };
            let mut upstream_headers = HeaderMap::new();
            upstream_headers.insert("content-type", HeaderValue::from_static("application/json"));
            if let Some((name, value)) = credential_header(&app, &provider) {
                upstream_headers.insert(name, value);
            }
            (
                format!("{}{path}", provider.base_url.trim_end_matches('/')),
                upstream_headers,
                Bytes::from(body),
                Some(info),
            )
        }
    };

    // Counted as usage from here on: everything that can refuse the request locally has passed, so every
    // remaining outcome is a real provider one (or waiting on it). A request that never gets a slot still
    // counts as a request and a failure, but the timer only starts once it is dispatched, below.
    let usage = app.gateway.usage_counters(&id);
    usage.add_request();
    let busy = Counted::new(&app.gateway.colony_counter(&colony));
    let stats = app.gateway.stats(&id);
    let timeout = Duration::from_secs(provider.timeout_secs());
    let permit: Option<OwnedSemaphorePermit> = match app.gateway.slots(&id, provider.max_concurrent) {
        None => None,
        Some(slots) => {
            let queue_timeout = provider.queue_timeout_secs();
            let waiting = Counted::new(&stats.queued);
            let acquired = tokio::time::timeout(Duration::from_secs(queue_timeout), slots.acquire_owned()).await;
            drop(waiting);
            match acquired {
                Ok(Ok(permit)) => Some(permit),
                _ => {
                    usage.add_failure_with_fallback(&provider);
                    return api_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "overloaded_error",
                        format!("provider \"{id}\" is busy: no free request slot within {queue_timeout} s"),
                        Some("queue_timeout"),
                    );
                }
            }
        }
    };
    let in_flight = Counted::new(&stats.in_flight);
    // The timer starts with the slot in hand, not before the wait for it: a request that queues out must
    // not add its queue time to `duration_ms`, which measures dispatched time only.
    let timed = Timed::new(usage.clone());

    let request = app.gateway.client.request(method, &url).headers(upstream_headers).body(body);
    let upstream = match tokio::time::timeout(timeout, request.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => {
            let reason = if e.is_connect() {
                "connection failed"
            } else {
                "request failed"
            };
            usage.add_failure_with_fallback(&provider);
            return api_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("provider \"{id}\" is unreachable ({reason}: {})", e.without_url()),
                Some("unreachable"),
            );
        }
        Err(_) => {
            usage.add_failure_with_fallback(&provider);
            return api_error(
                StatusCode::GATEWAY_TIMEOUT,
                "api_error",
                format!("provider \"{id}\" did not respond within {} s", timeout.as_secs()),
                Some("timeout"),
            );
        }
    };

    // The guards live as long as the body, so slots and activity cover the whole streamed response.
    let guards = (busy, in_flight, permit, timed);
    if let Some(info) = translation {
        let record = usage_recorder(&app, &colony, &provider);
        return openai_response(upstream, guards, usage, record, timeout, &info, &id).await;
    }

    let status = upstream.status();
    if status.as_u16() >= 400 {
        usage.add_failure();
    }
    let mut response_headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if !DROP_RESPONSE_HEADERS.contains(&name.as_str()) {
            response_headers.append(name.clone(), value.clone());
        }
    }
    // A GGUF server can sit silent for minutes during prefill before its first SSE event; stream_body
    // keeps the connection alive with comment lines in that case, which only makes sense for SSE:
    // injecting bytes into a non-streaming JSON body would corrupt it.
    let is_sse = response_headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("text/event-stream"));
    // The body forwards exactly as upstream sent it; the tap only watches a private copy for usage.
    let body = counted_body(
        stream_body(upstream.bytes_stream(), guards, timeout, is_sse),
        UsageTap::anthropic(is_sse),
        Some(usage_recorder(&app, &colony, &provider)),
    );
    let mut response = Response::new(Body::from_stream(body));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

/// An `openai`-wire provider's response in Anthropic's shape. Only `retry-after` is copied from upstream:
/// OpenAI's other headers (`openai-*`, `x-ratelimit-*`) describe a different API. Once response headers
/// have arrived there is no fallback, so none of these errors carries `x-colonizer-fallback`. The usage
/// the translation already extracted is teed out to `record_routed_usage` on both paths.
/// have arrived there is no fallback, so none of these errors carries `x-colonizer-fallback`.
async fn openai_response(
    upstream: reqwest::Response,
    guards: Guards,
    usage: Arc<UsageCounters>,
    record: Recorder,
    timeout: Duration,
    info: &openai::RequestInfo,
    id: &str,
) -> Response {
    let status = upstream.status();
    let retry_after = upstream.headers().get("retry-after").cloned();
    if status.is_success() && info.stream {
        let body = stream_body(
            openai::translate_stream(upstream.bytes_stream(), info.model.clone(), record),
            guards,
            timeout,
            true,
        );
        let mut response = Response::new(Body::from_stream(body));
        response
            .headers_mut()
            .insert("content-type", HeaderValue::from_static("text/event-stream"));
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-cache"));
        return response;
    }
    let bytes = match tokio::time::timeout(timeout, upstream.bytes()).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            usage.add_failure();
            return api_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("provider \"{id}\" response failed: {}", e.without_url()),
                None,
            );
        }
        Err(_) => {
            usage.add_failure();
            return api_error(
                StatusCode::GATEWAY_TIMEOUT,
                "api_error",
                format!("provider \"{id}\" did not finish its response within {} s", timeout.as_secs()),
                None,
            );
        }
    };
    drop(guards);
    let mut response = if status.is_success() {
        match openai::translate_response(&bytes, info) {
            Ok((message, priced)) => {
                record(priced);
                (StatusCode::OK, Json(message)).into_response()
            }
            Err(message) => api_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("provider \"{id}\": {message}"),
                None,
            ),
        }
    } else {
        if status.as_u16() >= 400 {
            usage.add_failure();
        }
        let (status, kind, message) = openai::translate_error(status, &bytes, id);
        api_error(status, kind, message, None)
    };
    if let Some(value) = retry_after {
        response.headers_mut().insert("retry-after", value);
    }
    response
}

/// Probes `GET {base_url}/v1/models` with the provider's credential.
pub async fn probe(app: &App, provider: &Provider) -> Value {
    let started = Instant::now();
    let mut request = app
        .gateway
        .client
        .get(format!("{}/v1/models", provider.base_url.trim_end_matches('/')))
        .timeout(HEALTH_TIMEOUT);
    if let Some((name, value)) = credential_header(app, provider) {
        request = request.header(name, value);
    }
    let checked_at = chrono::Utc::now();
    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            let models: Vec<Value> = body["data"]
                .as_array()
                .map(|data| data.iter().filter_map(|m| m["id"].as_str()).map(|id| json!(id)).collect())
                .unwrap_or_default();
            json!({
                "reachable": true,
                "status": status,
                "latency_ms": started.elapsed().as_millis() as u64,
                "models": models,
                "error": null,
                "checked_at": checked_at,
            })
        }
        Err(e) => {
            let error = if e.is_timeout() {
                format!("no response within {} s", HEALTH_TIMEOUT.as_secs())
            } else if e.is_connect() {
                "connection failed".to_string()
            } else {
                e.without_url().to_string()
            };
            json!({"reachable": false, "status": null, "latency_ms": null, "models": [], "error": error, "checked_at": checked_at})
        }
    }
}

pub async fn health(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let provider = app
        .providers()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such provider"))?;
    Ok(Json(probe(&app, &provider).await))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_urls_keep_the_base_path_and_reject_traversal() {
        assert_eq!(
            upstream_url("https://api.deepseek.com/anthropic/", "/v1/messages", Some("beta=true")).as_deref(),
            Some("https://api.deepseek.com/anthropic/v1/messages?beta=true")
        );
        assert_eq!(
            upstream_url("http://100.80.225.14:8000", "/v1/models", None).as_deref(),
            Some("http://100.80.225.14:8000/v1/models")
        );
        assert!(upstream_url("http://h", "/v1/../admin", None).is_none());
        assert!(upstream_url("http://h", "/v1/%2e%2e/admin", None).is_none());
        assert!(upstream_url("http://h", "v1/messages", None).is_none());
    }

    #[test]
    fn forwarded_headers_drop_colony_credentials_and_oauth_betas() {
        let mut incoming = HeaderMap::new();
        incoming.insert("authorization", HeaderValue::from_static("Bearer colony-placeholder"));
        incoming.insert("x-api-key", HeaderValue::from_static("placeholder"));
        incoming.insert(COLONY_HEADER, HeaderValue::from_static("token"));
        incoming.insert("content-type", HeaderValue::from_static("application/json"));
        incoming.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        incoming.insert(
            "anthropic-beta",
            HeaderValue::from_static("oauth-2025-04-20, interleaved-thinking-2025-05-14"),
        );
        let out = forward_headers(
            &incoming,
            Some((HeaderName::from_static("x-api-key"), HeaderValue::from_static("real"))),
        );
        assert_eq!(out.get("x-api-key").unwrap(), "real");
        assert!(out.get("authorization").is_none());
        assert!(out.get(COLONY_HEADER).is_none());
        assert_eq!(out.get("anthropic-beta").unwrap(), "interleaved-thinking-2025-05-14");
        assert_eq!(out.get("content-type").unwrap(), "application/json");

        let mut only_oauth = HeaderMap::new();
        only_oauth.insert("anthropic-beta", HeaderValue::from_static("oauth-2025-04-20"));
        assert!(forward_headers(&only_oauth, None).get("anthropic-beta").is_none());
    }

    /// A gateway whose usage file lives in a fresh temp directory.
    fn usage_gateway(dir: &std::path::Path) -> Gateway {
        Gateway::new(dir).unwrap()
    }

    fn provider(id: &str, fallback_model: Option<&str>) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            base_url: "http://127.0.0.1:9".into(),
            auth: "none".into(),
            wire: crate::providers::Wire::Anthropic,
            models: vec![],
            preset: "custom".into(),
            timeout_secs: None,
            max_concurrent: None,
            queue_timeout_secs: None,
            context_tokens: None,
            fallback_model: fallback_model.map(str::to_string),
            pricing: None,
        }
    }

    #[test]
    fn usage_counts_every_outcome_a_request_can_have() {
        let gateway = usage_gateway(&std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4())));
        let usage = gateway.usage_counters("strix");

        // A dispatched request that streamed back fine.
        usage.add_request();
        let timed = Timed::new(usage.clone());
        std::thread::sleep(Duration::from_millis(2));
        drop(timed);
        let after_success = usage.snapshot();
        assert_eq!(
            (after_success.requests, after_success.failures, after_success.fallbacks),
            (1, 0, 0)
        );
        assert!(after_success.last_request_at.is_some());

        // Each of the three gateway-level fallback errors, with a fallback model configured.
        let fallback = provider("strix", Some("sonnet"));
        for _ in 0..3 {
            usage.add_request();
            usage.add_failure_with_fallback(&fallback);
        }
        let after_errors = usage.snapshot();
        assert_eq!(
            (after_errors.requests, after_errors.failures, after_errors.fallbacks),
            (4, 3, 3)
        );

        // An upstream status >= 400 is a failure without a fallback answer.
        usage.add_request();
        usage.add_failure();
        let after_status = usage.snapshot();
        assert_eq!(
            (after_status.requests, after_status.failures, after_status.fallbacks),
            (5, 4, 3)
        );
        assert!(
            after_status.duration_ms > 0,
            "the dropped timer recorded the request's wall-clock time"
        );

        // Without a fallback model the failure is counted, the predicted fallback is not.
        usage.add_failure_with_fallback(&provider("strix", None));
        assert_eq!(usage.snapshot().fallbacks, 3);
        assert_eq!(usage.snapshot().failures, 5);
    }

    /// A request that queues past `queue_timeout_secs` never reaches the provider: it still counts as a
    /// request and a fallback failure, but `proxy` only creates the `Timed` guard once the slot is in hand,
    /// so the queue wait adds nothing to `duration_ms`.
    #[tokio::test]
    async fn a_queue_timeout_counts_a_request_and_a_failure_but_no_duration() {
        let gateway = usage_gateway(&std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4())));
        let usage = gateway.usage_counters("strix");
        // The provider's only slot is held by a request already in flight, and the one below is given no
        // queue time to speak of, so its wait gives up immediately.
        let held = gateway.slots("strix", Some(1)).unwrap().acquire_owned().await.unwrap();
        let queued_out = Provider {
            queue_timeout_secs: Some(0),
            ..provider("strix", Some("sonnet"))
        };

        // The queue-timeout path in `proxy`, in its order: the attempt counts before it waits, the wait
        // times out, and the failure carries the fallback prediction — with no timer covering any of it.
        usage.add_request();
        let acquired = tokio::time::timeout(
            Duration::from_secs(queued_out.queue_timeout_secs()),
            gateway.slots("strix", Some(1)).unwrap().acquire_owned(),
        )
        .await;
        assert!(acquired.is_err(), "the wait gives up while the first request holds the slot");
        usage.add_failure_with_fallback(&queued_out);

        let snapshot = usage.snapshot();
        assert_eq!((snapshot.requests, snapshot.failures, snapshot.fallbacks), (1, 1, 1));
        assert_eq!(
            snapshot.duration_ms, 0,
            "queued time is not dispatched time: nothing reached the provider"
        );
        drop(held);
    }

    /// A response whose headers arrived but whose body then failed still counts as a failure: the colony
    /// got no usable response. Counted once per request, never also at the header phase when the status
    /// was >= 400.
    #[tokio::test]
    async fn an_openai_body_that_fails_after_the_headers_still_counts_as_a_failure() {
        let usage = Arc::new(UsageCounters::default());
        let info = openai::RequestInfo {
            model: "gpt-5.5".into(),
            stream: false,
        };
        let broken_body = |status: u16| {
            let reset: futures_util::stream::Once<futures_util::future::Ready<Result<Bytes, std::io::Error>>> =
                futures_util::stream::once(futures_util::future::ready(Err(std::io::Error::other(
                    "connection reset mid-body",
                ))));
            reqwest::Response::from(
                axum::http::Response::builder()
                    .status(status)
                    .body(reqwest::Body::wrap_stream(reset))
                    .unwrap(),
            )
        };
        let response = openai_response(
            broken_body(200),
            guards(),
            usage.clone(),
            Box::new(|_| {}),
            Duration::from_secs(30),
            &info,
            "strix",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(usage.snapshot().failures, 1, "the body-phase failure is counted");

        // A >= 400 status whose body then fails must not count twice: the body-phase count is the only one.
        let response = openai_response(
            broken_body(500),
            guards(),
            usage.clone(),
            Box::new(|_| {}),
            Duration::from_secs(30),
            &info,
            "strix",
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            usage.snapshot().failures,
            2,
            "one per request, never a header-phase count on top of the body-phase one"
        );
    }

    #[test]
    fn usage_survives_a_restart_and_a_broken_file_degrades_to_defaults() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let gateway = usage_gateway(&dir);
        let usage = gateway.usage_counters("strix");
        usage.add_request();
        gateway.flush_usage();
        assert!(dir.join("provider-usage.json").exists(), "a dirty flush writes the file");

        let reopened = usage_gateway(&dir);
        let reopened_usage = reopened.usage("strix");
        assert_eq!(
            (reopened_usage.requests, reopened_usage.failures, reopened_usage.fallbacks),
            (1, 0, 0)
        );
        assert_eq!(reopened_usage.last_request_at, usage.snapshot().last_request_at);

        // Two providers dirty at once flush together, and the flush clears every flag: after it, removing
        // the file and flushing again must not write it back, since both providers were persisted.
        usage.add_request();
        gateway.usage_counters("loki").add_request();
        gateway.flush_usage();
        std::fs::remove_file(dir.join("provider-usage.json")).unwrap();
        gateway.flush_usage();
        assert!(
            !dir.join("provider-usage.json").exists(),
            "a flush with nothing left dirty does not write the file"
        );

        std::fs::write(dir.join("provider-usage.json"), "{not json").unwrap();
        assert_eq!(
            usage_gateway(&dir).usage("strix"),
            ProviderUsage::default(),
            "a corrupt file means empty counters"
        );
        assert_eq!(
            usage_gateway(&std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()))).usage("strix"),
            ProviderUsage::default(),
            "a missing file means empty counters"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn deleting_a_provider_forgets_its_usage_and_flushes_the_removal() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let gateway = usage_gateway(&dir);
        gateway.usage_counters("strix").add_request();
        gateway.flush_usage();
        gateway.forget_usage("strix");
        assert_eq!(
            usage_gateway(&dir).usage("strix"),
            ProviderUsage::default(),
            "the removal is written at once"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The flush loop, the shutdown flush and `forget_usage` can write at the same time, so every write
    /// gets its own tmp path: whatever the interleaving and whichever rename lands last, the file on disk
    /// is a complete snapshot that parses — never a half-overwritten one — and no tmp file is left behind.
    #[test]
    fn concurrent_writers_always_leave_a_parseable_usage_file() {
        let dir = std::env::temp_dir().join(format!("colonizer-usage-{}", uuid::Uuid::new_v4()));
        let gateway = Arc::new(usage_gateway(&dir));
        // Writers whose snapshots differ in size — ids padded to different lengths, the map growing as they
        // go — the interleaving that used to let a shorter write cut a longer one off mid-JSON.
        let threads: Vec<_> = (0..3)
            .map(|t| {
                let gateway = gateway.clone();
                std::thread::spawn(move || {
                    let pad = "x".repeat((t + 1) * 64);
                    for i in 0..40 {
                        gateway.usage_counters(&format!("w{t}-{i}-{pad}")).add_request();
                        gateway.write_usage();
                    }
                })
            })
            .chain(std::iter::once({
                let gateway = gateway.clone();
                std::thread::spawn(move || {
                    for i in 0..40 {
                        gateway.usage_counters(&format!("gone-{i}")).add_request();
                        gateway.forget_usage(&format!("gone-{i}"));
                    }
                })
            }))
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }

        let saved: BTreeMap<String, ProviderUsage> = serde_json::from_slice(
            &std::fs::read(dir.join("provider-usage.json")).expect("the last write left the file in place"),
        )
        .expect("the file always parses, whatever the interleaving");
        assert!(!saved.is_empty(), "the surviving providers are on disk");
        for (id, usage) in &saved {
            let kept = id.starts_with("w0-") || id.starts_with("w1-") || id.starts_with("w2-");
            assert!(kept || id.starts_with("gone-"), "unexpected provider {id}");
            assert_eq!(usage.requests, 1, "provider {id} kept its tally");
        }
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == "provider-usage.json"),
            "no tmp file is left behind"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn slots_follow_the_configured_limit() {
        let gateway = usage_gateway(&std::env::temp_dir().join(format!("colonizer-gateway-{}", uuid::Uuid::new_v4())));
        assert!(gateway.slots("local", None).is_none());
        let one = gateway.slots("local", Some(1)).unwrap();
        let held = one.clone().acquire_owned().await.unwrap();
        assert!(gateway.slots("local", Some(1)).unwrap().try_acquire().is_err());
        // Raising the limit takes effect immediately.
        assert!(gateway.slots("local", Some(2)).unwrap().try_acquire().is_ok());
        drop(held);

        let busy = Counted::new(&gateway.colony_counter("abc"));
        assert!(gateway.colony_busy("abc"));
        drop(busy);
        assert!(!gateway.colony_busy("abc"));
        assert!(!gateway.colony_busy("unknown"));
    }

    #[test]
    fn tokens_compare_in_constant_time() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    fn guards() -> Guards {
        (
            Counted::new(&Default::default()),
            Counted::new(&Default::default()),
            None,
            Timed::new(Default::default()),
        )
    }

    /// A mock provider stream that yields `items` spaced out by their delays.
    fn delayed_chunks(items: Vec<(Duration, Bytes)>) -> impl futures_util::Stream<Item = reqwest::Result<Bytes>> {
        futures_util::stream::unfold(items.into_iter(), |mut it| async move {
            let (delay, chunk) = it.next()?;
            tokio::time::sleep(delay).await;
            Some((Ok(chunk), it))
        })
    }

    fn openai_chunk(delta: &str, finish_reason: &str) -> Bytes {
        Bytes::from(format!(
            "data: {{\"id\":\"c\",\"choices\":[{{\"delta\":{delta},\"finish_reason\":{finish_reason}}}]}}\n\n"
        ))
    }

    #[tokio::test(start_paused = true)]
    async fn translated_streams_still_get_pinged_through_a_silent_prefill() {
        let upstream = delayed_chunks(vec![
            (Duration::ZERO, openai_chunk(r#"{"role":"assistant"}"#, "null")),
            (
                Duration::from_secs(40),
                [
                    openai_chunk(r#"{"content":"hi"}"#, "\"stop\""),
                    Bytes::from_static(b"data: [DONE]\n\n"),
                ]
                .concat()
                .into(),
            ),
        ]);
        let body = stream_body(
            openai::translate_stream(upstream, "gpt-5.5".into(), |_| {}),
            guards(),
            Duration::from_secs(120),
            true,
        );
        tokio::pin!(body);

        let (mut pings, mut out) = (0, Vec::new());
        while let Some(chunk) = body.next().await {
            let chunk = chunk.unwrap();
            if chunk.as_ref() == SSE_PING {
                pings += 1;
                assert!(out.is_empty() || out.ends_with(b"\n\n"), "a ping landed inside an event");
            } else {
                out.extend_from_slice(&chunk);
            }
        }
        assert_eq!(pings, 2);
        assert!(out.ends_with(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
    }

    /// Upstream chunks that translate to nothing still reset the silence deadline. Here the provider talks
    /// every 10 s for 40 s against a 25 s timeout; counting only translated bytes would kill it at 25 s.
    #[tokio::test(start_paused = true)]
    async fn untranslatable_chunks_still_count_as_activity() {
        let empty = || (Duration::from_secs(10), openai_chunk("{}", "null"));
        let upstream = delayed_chunks(vec![
            (Duration::ZERO, openai_chunk(r#"{"role":"assistant"}"#, "null")),
            empty(),
            empty(),
            (Duration::from_secs(10), Bytes::from_static(b": OPENROUTER PROCESSING\n\n")),
            (
                Duration::from_secs(10),
                [
                    openai_chunk(r#"{"content":"done"}"#, "\"stop\""),
                    Bytes::from_static(b"data: [DONE]\n\n"),
                ]
                .concat()
                .into(),
            ),
        ]);
        let body = stream_body(
            openai::translate_stream(upstream, "gpt-5.5".into(), |_| {}),
            guards(),
            Duration::from_secs(25),
            true,
        );
        tokio::pin!(body);
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            out.extend_from_slice(&chunk.expect("the stream must not time out"));
        }
        assert!(std::str::from_utf8(&out).unwrap().contains("event: message_stop"));
    }

    #[tokio::test(start_paused = true)]
    async fn sse_streams_get_pinged_through_a_silent_prefill() {
        let chunks = delayed_chunks(vec![
            (Duration::ZERO, Bytes::from_static(b"event: message_start\n\n")),
            // Long enough silence to cross two SSE_PING_INTERVAL (15s) ticks before the real byte.
            (Duration::from_secs(40), Bytes::from_static(b"event: message_stop\n\n")),
        ]);
        let body = stream_body(chunks, guards(), Duration::from_secs(120), true);
        tokio::pin!(body);

        let mut pings = 0;
        let mut reals = 0;
        while let Some(chunk) = body.next().await {
            if chunk.unwrap().as_ref() == SSE_PING {
                pings += 1;
            } else {
                reals += 1;
            }
        }
        assert_eq!(reals, 2, "both real chunks should still arrive");
        assert_eq!(
            pings, 2,
            "one ping per 15s tick of the 40s silent gap, not reset by pings themselves"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn non_sse_responses_never_get_pinged() {
        let chunks = delayed_chunks(vec![(Duration::from_secs(40), Bytes::from_static(b"{\"ok\":true}"))]);
        let body = stream_body(chunks, guards(), Duration::from_secs(120), false);
        tokio::pin!(body);
        let chunk = body.next().await.unwrap().unwrap();
        assert_eq!(chunk.as_ref(), b"{\"ok\":true}");
        assert!(body.next().await.is_none());
    }

    /// Drains `chunks` through a counting body over an Anthropic SSE tap, returning the bytes the colony
    /// would receive and the usage the tap read.
    async fn counted(chunks: Vec<std::io::Result<Bytes>>, is_sse: bool) -> (Vec<u8>, Option<Usage>) {
        let seen = Arc::new(Mutex::new(None));
        let sink = seen.clone();
        let body = counted_body(
            futures_util::stream::iter(chunks),
            UsageTap::anthropic(is_sse),
            Some(Box::new(move |usage| *sink.lock().unwrap() = Some(usage))),
        );
        tokio::pin!(body);
        let mut forwarded = Vec::new();
        while let Some(chunk) = body.next().await {
            forwarded.extend_from_slice(&chunk.expect("the body forwards"));
        }
        let counted = seen.lock().unwrap().take();
        (forwarded, counted)
    }

    fn message_start(input: u64, cache_read: u64, cache_write: u64) -> String {
        format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":{input},\
             \"cache_read_input_tokens\":{cache_read},\"cache_creation_input_tokens\":{cache_write},\"output_tokens\":1}}}}}}\n\n"
        )
    }

    fn message_delta(output: u64) -> String {
        format!(
            "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":{output}}}}}\n\n"
        )
    }

    #[tokio::test]
    async fn an_anthropic_stream_is_counted_without_its_bytes_being_touched() {
        let body = message_start(25, 40, 5)
            + "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n"
            + &message_delta(15);
        let (forwarded, counted) = counted(vec![Ok(Bytes::from(body.clone()))], true).await;
        assert_eq!(
            String::from_utf8_lossy(&forwarded),
            body,
            "the colony receives exactly the bytes upstream sent"
        );
        assert_eq!(
            counted,
            Some(Usage {
                input_tokens: 25,
                output_tokens: 15,
                cache_read_tokens: 40,
                cache_write_tokens: 5
            })
        );
    }

    #[tokio::test]
    async fn counting_survives_chunks_split_mid_event() {
        let body = message_start(25, 40, 5) + &message_delta(15);
        let pieces: Vec<std::io::Result<Bytes>> = body
            .as_bytes()
            .chunks(7)
            .map(|piece| Ok(Bytes::copy_from_slice(piece)))
            .collect();
        let (forwarded, counted) = counted(pieces, true).await;
        assert_eq!(String::from_utf8_lossy(&forwarded), body);
        assert_eq!(
            counted,
            Some(Usage {
                input_tokens: 25,
                output_tokens: 15,
                cache_read_tokens: 40,
                cache_write_tokens: 5
            }),
            "a data line split across chunks is waited for, not half-read"
        );
    }

    #[tokio::test]
    async fn an_anthropic_json_body_is_counted_and_passes_through_unchanged() {
        let body = br#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","usage":{"input_tokens":12,"cache_read_input_tokens":80,"cache_creation_input_tokens":2,"output_tokens":9}}"#;
        let (forwarded, counted) = counted(vec![Ok(Bytes::from_static(body))], false).await;
        assert_eq!(
            forwarded,
            body.to_vec(),
            "the colony receives exactly the bytes upstream sent"
        );
        assert_eq!(
            counted,
            Some(Usage {
                input_tokens: 12,
                output_tokens: 9,
                cache_read_tokens: 80,
                cache_write_tokens: 2
            })
        );
    }

    #[tokio::test]
    async fn a_malformed_or_truncated_body_counts_nothing_and_still_forwards() {
        // A data line that never becomes valid JSON, and a stream cut off mid-event, both account zero.
        for (body, is_sse) in [
            (
                b"event: message_start\ndata: not json\n\nevent: message_delta\ndata: {}\n\n".as_slice(),
                true,
            ),
            (b"event: message_start\ndata: {\"type\":\"message_star".as_slice(), true),
            (b"<html>gateway error</html>".as_slice(), false),
            (b" &".as_slice(), false),
        ] {
            let (forwarded, counted) = counted(vec![Ok(Bytes::from_static(body))], is_sse).await;
            assert_eq!(forwarded, body.to_vec(), "bytes must pass unchanged no matter what they say");
            assert_eq!(counted, None, "no spend is recorded for {body:?}");
        }
    }

    /// `pings` are injected by `stream_body` upstream of the tap, so the tap must read past them.
    #[tokio::test]
    async fn keep_alive_pings_do_not_confuse_the_counting() {
        let body = message_start(10, 0, 0) + ": keep-alive\n\n" + &message_delta(4);
        let (forwarded, counted) = counted(vec![Ok(Bytes::from(body.clone()))], true).await;
        assert_eq!(String::from_utf8_lossy(&forwarded), body);
        assert_eq!(
            counted,
            Some(Usage {
                input_tokens: 10,
                output_tokens: 4,
                ..Default::default()
            })
        );
    }

    #[tokio::test]
    async fn an_empty_body_records_nothing_at_all() {
        let (forwarded, counted) = counted(vec![], true).await;
        assert!(forwarded.is_empty());
        assert_eq!(counted, None, "no body end callback fires for a body that never arrived");
    }

    #[tokio::test(start_paused = true)]
    async fn pings_never_land_inside_a_partly_forwarded_event() {
        let chunks = delayed_chunks(vec![
            (
                Duration::ZERO,
                Bytes::from_static(b"event: content_block_delta\ndata: {\"delta\":"),
            ),
            (Duration::from_secs(40), Bytes::from_static(b"\"hi\"}\n\n")),
            (Duration::from_secs(40), Bytes::from_static(b"event: message_stop\n\n")),
        ]);
        let body = stream_body(chunks, guards(), Duration::from_secs(120), true);
        tokio::pin!(body);
        let mut forwarded = Vec::new();
        while let Some(chunk) = body.next().await {
            forwarded.extend_from_slice(&chunk.unwrap());
        }
        let expected =
            b"event: content_block_delta\ndata: {\"delta\":\"hi\"}\n\n: keep-alive\n\n: keep-alive\n\nevent: message_stop\n\n";
        assert_eq!(String::from_utf8_lossy(&forwarded), String::from_utf8_lossy(expected));
    }

    #[tokio::test(start_paused = true)]
    async fn a_provider_silent_past_the_overall_timeout_still_errors_despite_pings() {
        let chunks = delayed_chunks(vec![(Duration::from_secs(600), Bytes::from_static(b"too late"))]);
        let body = stream_body(chunks, guards(), Duration::from_secs(50), true);
        tokio::pin!(body);
        let mut pings = 0;
        loop {
            match body.next().await.unwrap() {
                Ok(chunk) => {
                    assert_eq!(chunk.as_ref(), SSE_PING);
                    pings += 1;
                }
                Err(e) => {
                    assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
                    break;
                }
            }
        }
        assert!(pings >= 3, "expected pings while waiting, got {pings}");
    }
}
