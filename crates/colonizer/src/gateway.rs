//! Provider gateway (docs/protocol.md §6.5). Colonies send routed model requests here instead of
//! straight to the provider: the mothership is on the operator's networks (tailnet, LAN), holds the
//! provider keys, and sees every colony, so it can queue requests per provider, apply long timeouts,
//! and report which colonies are waiting on a model. Colonies authenticate with a per-colony token.

use crate::{
    client_error,
    providers::{strip_oauth_betas, Provider},
    util::read_trimmed,
    ApiResult, App, Shared,
};
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
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
const DROP_RESPONSE_HEADERS: [&str; 6] = ["connection", "keep-alive", "proxy-connection", "transfer-encoding", "upgrade", "content-length"];
/// How long an SSE response may go silent before the gateway sends a comment line to keep the
/// connection (and any byte-level idle watchdog downstream) alive during a long prefill.
const SSE_PING_INTERVAL: Duration = Duration::from_secs(15);
const SSE_PING: &[u8] = b": keep-alive\n\n";

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

#[derive(Default)]
struct ProviderStats {
    in_flight: Arc<AtomicU64>,
    queued: Arc<AtomicU64>,
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
}

impl Gateway {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { client, stats: Default::default(), limits: Default::default(), colonies: Default::default() })
    }

    fn stats(&self, provider: &str) -> Arc<ProviderStats> {
        self.stats.lock().unwrap().entry(provider.to_string()).or_default().clone()
    }

    /// `(in_flight, queued)` for a provider across all colonies.
    pub fn load(&self, provider: &str) -> (u64, u64) {
        let stats = self.stats(provider);
        (stats.in_flight.load(Ordering::SeqCst), stats.queued.load(Ordering::SeqCst))
    }

    fn colony_counter(&self, colony: &str) -> Arc<AtomicU64> {
        self.colonies.lock().unwrap().entry(colony.to_string()).or_default().clone()
    }

    /// True while the colony is waiting on a model through the gateway; the watchdog counts that as progress.
    pub fn colony_busy(&self, colony: &str) -> bool {
        self.colonies.lock().unwrap().get(colony).is_some_and(|c| c.load(Ordering::SeqCst) > 0)
    }

    /// The provider's request slots, or `None` when it has no concurrency limit. A changed limit gets a
    /// fresh semaphore; requests holding the old one finish without counting against the new limit.
    fn slots(&self, provider: &str, max: Option<u64>) -> Option<Arc<Semaphore>> {
        let mut limits = self.limits.lock().unwrap();
        let Some(max) = max else {
            limits.remove(provider);
            return None;
        };
        let limit = limits
            .entry(provider.to_string())
            .or_insert_with(|| Limit { max, slots: Arc::new(Semaphore::new(max as usize)) });
        if limit.max != max {
            *limit = Limit { max, slots: Arc::new(Semaphore::new(max as usize)) };
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
            .find(|s| read_trimmed(&self.gateway_token_file(&s.id)).is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes())))
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

/// An error in Anthropic's shape, so Claude Code reports it like any API error.
fn api_error(status: StatusCode, kind: &str, message: impl Into<String>, fallback: Option<&'static str>) -> Response {
    let mut response = (status, Json(json!({"type": "error", "error": {"type": kind, "message": message.into()}}))).into_response();
    if let Some(reason) = fallback {
        response.headers_mut().insert(FALLBACK_HEADER, HeaderValue::from_static(reason));
    }
    response
}

/// Busy/in-flight counters and the provider's concurrency permit, held for as long as the response body.
type Guards = (Counted, Counted, Option<OwnedSemaphorePermit>);

/// Streams `chunks` downstream, translating provider errors and enforcing `timeout` as an overall
/// silence deadline. For an SSE response (`is_sse`), a `: keep-alive` comment line — ignored by any
/// spec-compliant SSE parser — is sent every `SSE_PING_INTERVAL` of silence, so the connection and any
/// byte-level idle watchdog downstream see activity through a long prefill. A ping doesn't reset the
/// deadline: a provider that never produces a real byte still times out after `timeout`.
fn stream_body(
    chunks: impl futures_util::Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
    guards: Guards,
    timeout: Duration,
    is_sse: bool,
) -> impl futures_util::Stream<Item = std::io::Result<Bytes>> {
    let chunks = Box::pin(chunks);
    // tokio::time::Instant (not std::time::Instant) so this respects a paused clock under test.
    futures_util::stream::unfold((chunks, Some(guards), tokio::time::Instant::now()), move |(mut chunks, guards, silent_since)| async move {
        guards.as_ref()?;
        let remaining = timeout.saturating_sub(silent_since.elapsed());
        if remaining.is_zero() {
            return Some((Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "provider went silent")), (chunks, None, silent_since)));
        }
        let wait = if is_sse { remaining.min(SSE_PING_INTERVAL) } else { remaining };
        match tokio::time::timeout(wait, chunks.next()).await {
            Ok(Some(Ok(chunk))) => Some((Ok(chunk), (chunks, guards, tokio::time::Instant::now()))),
            Ok(Some(Err(e))) => Some((Err(std::io::Error::other(e.without_url())), (chunks, None, silent_since))),
            Ok(None) => None,
            Err(_) if is_sse && wait < remaining => Some((Ok(Bytes::from_static(SSE_PING)), (chunks, guards, silent_since))),
            Err(_) => Some((Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "provider went silent")), (chunks, None, silent_since))),
        }
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
        if let Ok(value) = HeaderValue::from_str(&betas) {
            if !betas.is_empty() {
                out.insert("anthropic-beta", value);
            }
        }
    }
    if let Some((name, value)) = credential {
        out.insert(name, value);
    }
    out
}

async fn proxy(State(app): State<Shared>, Path((id, _)): Path<(String, String)>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let token = headers.get(COLONY_HEADER).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let Some(colony) = app.colony_for_token(token).await else {
        return api_error(StatusCode::UNAUTHORIZED, "authentication_error", "colonizer gateway: unknown colony", None);
    };
    let Some(provider) = app.providers().into_iter().find(|p| p.id == id) else {
        return api_error(StatusCode::NOT_FOUND, "not_found_error", format!("colonizer gateway: no provider \"{id}\""), None);
    };
    let rest = uri.path().strip_prefix(&format!("/providers/{id}")).unwrap_or_default();
    let Some(url) = upstream_url(&provider.base_url, rest, uri.query()) else {
        return api_error(StatusCode::BAD_REQUEST, "invalid_request_error", "colonizer gateway: unsupported path", None);
    };

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
                    return api_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "overloaded_error",
                        format!("provider \"{id}\" is busy: no free request slot within {queue_timeout} s"),
                        Some("queue_timeout"),
                    )
                }
            }
        }
    };
    let in_flight = Counted::new(&stats.in_flight);

    let request = app
        .gateway
        .client
        .request(method, &url)
        .headers(forward_headers(&headers, credential_header(&app, &provider)))
        .body(body);
    let upstream = match tokio::time::timeout(timeout, request.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => {
            let reason = if e.is_connect() { "connection failed" } else { "request failed" };
            return api_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("provider \"{id}\" is unreachable ({reason}: {})", e.without_url()),
                Some("unreachable"),
            );
        }
        Err(_) => {
            return api_error(
                StatusCode::GATEWAY_TIMEOUT,
                "api_error",
                format!("provider \"{id}\" did not respond within {} s", timeout.as_secs()),
                Some("timeout"),
            );
        }
    };

    let status = upstream.status();
    let mut response_headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if !DROP_RESPONSE_HEADERS.contains(&name.as_str()) {
            response_headers.append(name.clone(), value.clone());
        }
    }
    // A GGUF server can sit silent for minutes during prefill before its first SSE event; stream_body
    // keeps the connection alive with comment lines in that case, which only makes sense for SSE:
    // injecting bytes into a non-streaming JSON body would corrupt it.
    let is_sse = response_headers.get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|c| c.contains("text/event-stream"));
    // The guards live as long as the body, so slots and activity cover the whole streamed response.
    let guards = (busy, in_flight, permit);
    let body = stream_body(upstream.bytes_stream(), guards, timeout, is_sse);
    let mut response = Response::new(Body::from_stream(body));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

/// Probes `GET {base_url}/v1/models` with the provider's credential.
pub async fn probe(app: &App, provider: &Provider) -> Value {
    let started = Instant::now();
    let mut request = app.gateway.client.get(format!("{}/v1/models", provider.base_url.trim_end_matches('/'))).timeout(HEALTH_TIMEOUT);
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
        assert_eq!(upstream_url("http://100.80.225.14:8000", "/v1/models", None).as_deref(), Some("http://100.80.225.14:8000/v1/models"));
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
        incoming.insert("anthropic-beta", HeaderValue::from_static("oauth-2025-04-20, interleaved-thinking-2025-05-14"));
        let out = forward_headers(&incoming, Some((HeaderName::from_static("x-api-key"), HeaderValue::from_static("real"))));
        assert_eq!(out.get("x-api-key").unwrap(), "real");
        assert!(out.get("authorization").is_none());
        assert!(out.get(COLONY_HEADER).is_none());
        assert_eq!(out.get("anthropic-beta").unwrap(), "interleaved-thinking-2025-05-14");
        assert_eq!(out.get("content-type").unwrap(), "application/json");

        let mut only_oauth = HeaderMap::new();
        only_oauth.insert("anthropic-beta", HeaderValue::from_static("oauth-2025-04-20"));
        assert!(forward_headers(&only_oauth, None).get("anthropic-beta").is_none());
    }

    #[tokio::test]
    async fn slots_follow_the_configured_limit() {
        let gateway = Gateway::new().unwrap();
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
        (Counted::new(&Default::default()), Counted::new(&Default::default()), None)
    }

    /// A mock provider stream that yields `items` spaced out by their delays.
    fn delayed_chunks(items: Vec<(Duration, Bytes)>) -> impl futures_util::Stream<Item = reqwest::Result<Bytes>> {
        futures_util::stream::unfold(items.into_iter(), |mut it| async move {
            let (delay, chunk) = it.next()?;
            tokio::time::sleep(delay).await;
            Some((Ok(chunk), it))
        })
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
        assert_eq!(pings, 2, "one ping per 15s tick of the 40s silent gap, not reset by pings themselves");
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
