//! Jev: an optional, default-off external classifier consulted for a second opinion on model-tier
//! routing, in shadow mode only. Its opinion is fetched here, in the async boot path, before
//! `routing::decide` runs, and attached to `routing::Signals` as plain data — `decide` itself stays a
//! synchronous, pure function with no network call inside it, and nothing in this file ever changes
//! the tier a colony runs on.
//!
//! The vendor's specific claims — the endpoint, its pricing, its latency — could not be independently
//! verified while this was written; treat every number below as a reasonable default this integration
//! chose, not a fact it depends on. The safety property that matters holds either way: with no
//! `JEV_API_KEY` secret and no `jev_shadow_mode` setting on, [`shadow_opinion`] makes zero network
//! calls and always returns `None`. An unverified, misconfigured or even nonexistent vendor degrades
//! to "no opinion, every time" — it never blocks boot, slows it meaningfully, or errors out.

use crate::routing::{JevOpinion, Signals, Tier};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

/// The Jev model this integration is pinned to. Never "jev-latest": a second opinion's thresholds and
/// calibration are specific to one model version and are not assumed to carry over to another.
pub const JEV_MODEL: &str = "jev-1.13.0";

/// The claimed endpoint. Unverified as of this writing — see the module doc.
const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// A rough, unverified per-input-token price (output is claimed free), used only to log a rough cost
/// estimate alongside each opinion — never metered billing, and never folded into a colony's own
/// routed model cost, since this opinion never picks a model.
pub const JEV_PRICE_PER_MTOK_USD: f64 = 0.042;

/// The one question asked per task: a `choice` between the three tiers. A `choice` question returns
/// per-option probabilities plus a single confidence for the winner, which is exactly enough to take
/// as the opinion by argmax — no hand-picked threshold needed, and thresholds must never be shared
/// across question types (`choice`, `score`, `noul`) or across model versions.
const QUESTION_ID: &str = "tier";
const CHOICES: [&str; 3] = ["low", "medium", "high"];

/// The condensed, metadata-only state sent to Jev. Never file contents, never the raw body text and
/// never credentials — the title has credential-looking tokens redacted, and the body contributes
/// only a bucketed size.
#[derive(Debug, Serialize)]
pub(crate) struct CondensedState {
    title: String,
    labels: Vec<String>,
    body_size: &'static str,
    checklist_items: usize,
    paths: usize,
    one_directory: bool,
    known_preset: bool,
}

impl CondensedState {
    fn build(title: &str, labels: &[String], signals: &Signals) -> Self {
        CondensedState {
            title: redact(title),
            labels: labels.to_vec(),
            body_size: body_bucket(signals.body_chars),
            checklist_items: signals.checklist_items,
            paths: signals.paths,
            one_directory: signals.one_directory,
            known_preset: signals.known_preset,
        }
    }
}

/// Buckets a character count into a coarse size, so a raw body length never leaves the boot path.
fn body_bucket(chars: usize) -> &'static str {
    match chars {
        0..=499 => "small",
        500..=2_999 => "medium",
        3_000..=9_999 => "large",
        _ => "huge",
    }
}

/// Strips tokens that look like credentials before text leaves the boot path: each whitespace-
/// separated token over 20 characters is redacted when it starts with a known secret prefix, or when
/// it is otherwise a long unbroken run of the characters a base64 or URL-safe token is made of.
fn redact(input: &str) -> String {
    input
        .split_whitespace()
        .map(|token| if looks_like_a_secret(token) { "[redacted]" } else { token })
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_a_secret(token: &str) -> bool {
    const PREFIXES: [&str; 5] = ["ghp_", "sk-", "xox", "AKIA", "Bearer"];
    token.len() > 20
        && (PREFIXES.iter().any(|prefix| token.starts_with(prefix))
            || token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=')))
}

/// A small HTTP client owned by this feature alone, following the crate's convention of not sharing a
/// `reqwest::Client` across features (see `gateway.rs`'s `Gateway.client`, `mem0.rs`'s `Mem0.client`).
pub struct JevClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl JevClient {
    pub fn new(api_key: String) -> Self {
        Self::build(api_key, ENDPOINT.to_string())
    }

    #[cfg(test)]
    fn with_base_url(api_key: String, base_url: String) -> Self {
        Self::build(api_key, base_url)
    }

    fn build(api_key: String, base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client with only a user agent set should always build");
        JevClient {
            client,
            api_key,
            base_url,
        }
    }

    /// Asks Jev for a tier opinion on one task's condensed state. Never returns `Err`: any failure —
    /// a missing/malformed response, a non-2xx status, a network error, or running past the hard
    /// timeout — resolves to `None`, so a caller can never be tempted to let it fail boot.
    pub async fn ask(&self, state: &CondensedState) -> Option<JevOpinion> {
        tokio::time::timeout(Duration::from_millis(1800), self.ask_inner(state))
            .await
            .ok()
            .flatten()
    }

    async fn ask_inner(&self, state: &CondensedState) -> Option<JevOpinion> {
        let serialized = serde_json::to_string(state).ok()?;
        let body = json!({
            "model": JEV_MODEL,
            "question": {
                "id": QUESTION_ID,
                "type": "choice",
                "options": CHOICES,
            },
            "state": state,
        });
        let mut backoff = Duration::from_millis(150);
        // Up to 3 attempts total: the first, and 2 retries on 429/529 with backoff 150ms then 300ms.
        for attempt in 0..3 {
            let response = self
                .client
                .post(&self.base_url)
                .timeout(Duration::from_millis(800))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&body)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                // A network error, a connect failure or a per-attempt timeout: none of these are the
                // rate-limit/overload signal that is worth a retry, so this is the last attempt.
                Err(_) => return None,
            };
            let status = response.status();
            if status.is_success() {
                let value: Value = response.json().await.ok()?;
                return parse_opinion(&value, &serialized);
            }
            let retryable = matches!(status.as_u16(), 429 | 529);
            if retryable && attempt < 2 {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }
            return None;
        }
        None
    }
}

/// Defensively parses a response: the vendor is unverified, so nothing here assumes the exact real
/// response shape. Looks for a top-level `model` string and an `answers` object carrying this
/// question's id with a `choice` string and a `confidence` number — anything missing or malformed is
/// a failed call, not a panic or an error.
fn parse_opinion(value: &Value, request_json: &str) -> Option<JevOpinion> {
    let model = value.get("model")?.as_str()?.to_string();
    let answer = value.get("answers")?.get(QUESTION_ID)?;
    let tier = Tier::parse(answer.get("choice")?.as_str()?)?;
    let confidence = answer.get("confidence")?.as_f64()?;
    // A rough token estimate from the request payload's size (~4 characters per token), times the
    // unverified price above — not metered billing, just enough to keep the cost of asking visible.
    let estimated_tokens = request_json.len() as f64 / 4.0;
    let estimated_cost_usd = estimated_tokens * JEV_PRICE_PER_MTOK_USD / 1_000_000.0;
    Some(JevOpinion {
        tier,
        model,
        confidence,
        estimated_cost_usd,
    })
}

/// The key a real account holder would have set themselves, filtered so blank counts as unset.
fn valid_key(raw: Option<String>) -> Option<String> {
    raw.filter(|key| !key.trim().is_empty())
}

/// The mothership's own TypeSafe key, shared by the Jev features that call TypeSafe from a colony.
pub fn api_key() -> Option<String> {
    valid_key(std::env::var("JEV_API_KEY").ok())
}

/// The entry point the boot path calls before `routing::decide`. Two short-circuits, both silent and
/// both make zero network calls: the setting is off, or there is no usable `JEV_API_KEY`. Either way
/// a missing/disabled Jev is a clean "no opinion", never an error.
pub async fn shadow_opinion(enabled: bool, title: &str, labels: &[String], signals: &Signals) -> Option<JevOpinion> {
    if !enabled {
        return None;
    }
    let api_key = api_key()?;
    let state = CondensedState::build(title, labels, signals);
    JevClient::new(api_key).ask(&state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Instant;

    fn good_body() -> Value {
        json!({
            "model": "jev-1.13.0",
            "answers": {"tier": {"choice": "high", "confidence": 0.87}},
        })
    }

    #[derive(Clone, Copy)]
    enum Mode {
        Ok,
        FailOnceThenOk,
        AlwaysStatus(u16),
        NeverRespond,
    }

    #[derive(Clone)]
    struct MockState {
        mode: Mode,
        attempts: Arc<AtomicUsize>,
    }

    async fn handle(State(state): State<MockState>, Json(_body): Json<Value>) -> axum::response::Response {
        let attempt = state.attempts.fetch_add(1, Ordering::SeqCst);
        match state.mode {
            Mode::Ok => (axum::http::StatusCode::OK, Json(good_body())).into_response(),
            Mode::FailOnceThenOk => {
                if attempt == 0 {
                    axum::http::StatusCode::TOO_MANY_REQUESTS.into_response()
                } else {
                    (axum::http::StatusCode::OK, Json(good_body())).into_response()
                }
            }
            Mode::AlwaysStatus(code) => axum::http::StatusCode::from_u16(code).unwrap().into_response(),
            Mode::NeverRespond => {
                tokio::time::sleep(Duration::from_secs(10)).await;
                axum::http::StatusCode::OK.into_response()
            }
        }
    }

    /// Serves the mock on a loopback port and returns the full URL `JevClient` should post to.
    async fn serve(mode: Mode) -> (String, Arc<AtomicUsize>) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let state = MockState {
            mode,
            attempts: attempts.clone(),
        };
        let router = Router::new().route("/v1/systemone", post(handle)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{addr}/v1/systemone"), attempts)
    }

    fn state() -> CondensedState {
        CondensedState {
            title: "fix a typo".to_string(),
            labels: vec!["typo".to_string()],
            body_size: "small",
            checklist_items: 0,
            paths: 1,
            one_directory: true,
            known_preset: true,
        }
    }

    #[tokio::test]
    async fn a_200_response_with_a_valid_body_returns_the_opinion_it_carries() {
        let (base, _) = serve(Mode::Ok).await;
        let client = JevClient::with_base_url("test-key".into(), base);
        let opinion = client.ask(&state()).await.expect("expected an opinion");
        assert_eq!(opinion.tier, Tier::High);
        assert_eq!(opinion.model, "jev-1.13.0");
        assert_eq!(opinion.confidence, 0.87);
        assert!(opinion.estimated_cost_usd > 0.0);
    }

    #[tokio::test]
    async fn a_429_once_then_a_200_still_returns_the_opinion_proving_the_retry_worked() {
        let (base, attempts) = serve(Mode::FailOnceThenOk).await;
        let client = JevClient::with_base_url("test-key".into(), base);
        let opinion = client.ask(&state()).await.expect("expected the retry to succeed");
        assert_eq!(opinion.tier, Tier::High);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_529_every_time_exhausts_the_retries_and_returns_none_well_under_the_hard_ceiling() {
        let (base, attempts) = serve(Mode::AlwaysStatus(529)).await;
        let client = JevClient::with_base_url("test-key".into(), base);
        let started = Instant::now();
        let opinion = client.ask(&state()).await;
        let elapsed = started.elapsed();
        assert!(opinion.is_none());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        // Backoff alone is 150ms + 300ms = 450ms; loopback requests add very little on top of that,
        // so this proves the retries do not silently run all the way out to the 1.8s hard timeout.
        assert!(elapsed < Duration::from_millis(1500), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn an_endpoint_that_never_responds_returns_none_and_does_not_hang() {
        let (base, _) = serve(Mode::NeverRespond).await;
        let client = JevClient::with_base_url("test-key".into(), base);
        let started = Instant::now();
        let opinion = client.ask(&state()).await;
        let elapsed = started.elapsed();
        assert!(opinion.is_none());
        // Bounded well below the 1.8s hard ceiling (the 800ms per-attempt timeout fires first), and
        // nowhere near indefinite.
        assert!(elapsed < Duration::from_millis(1800), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn shadow_opinion_with_the_feature_disabled_makes_no_call_and_returns_none() {
        let signals = crate::routing::signals("fix a typo", "short body", &["typo".to_string()], true);
        let opinion = shadow_opinion(false, "fix a typo", &["typo".to_string()], &signals).await;
        assert!(opinion.is_none());
    }

    #[test]
    fn a_missing_or_blank_key_counts_as_unset_and_a_real_one_does_not() {
        assert_eq!(valid_key(None), None);
        assert_eq!(valid_key(Some(String::new())), None);
        assert_eq!(valid_key(Some("   ".to_string())), None);
        assert_eq!(valid_key(Some("real-key".to_string())), Some("real-key".to_string()));
    }

    #[test]
    fn body_bucket_flips_at_each_boundary() {
        assert_eq!(body_bucket(0), "small");
        assert_eq!(body_bucket(499), "small");
        assert_eq!(body_bucket(500), "medium");
        assert_eq!(body_bucket(2_999), "medium");
        assert_eq!(body_bucket(3_000), "large");
        assert_eq!(body_bucket(9_999), "large");
        assert_eq!(body_bucket(10_000), "huge");
    }

    #[test]
    fn redact_strips_a_known_secret_prefix_wherever_it_appears_in_the_text() {
        assert_eq!(
            redact("token ghp_abcdefghijklmnopqrstuvwxyz1234 leaked"),
            "token [redacted] leaked"
        );
        assert_eq!(redact("key sk-abcdefghijklmnopqrstuvwxyz123456 here"), "key [redacted] here");
    }

    #[test]
    fn redact_strips_a_long_unbroken_token_shaped_run_with_no_known_prefix() {
        let long_token = "a".repeat(21) + "Zz09_-+/=";
        let text = format!("value {long_token} end");
        assert_eq!(redact(&text), "value [redacted] end");
    }

    #[test]
    fn redact_leaves_ordinary_prose_and_a_token_at_exactly_the_length_bound_alone() {
        assert_eq!(
            redact("fix the bug in src/main.rs please"),
            "fix the bug in src/main.rs please"
        );
        // Exactly 20 characters: the bound is "longer than 20", so this one is left alone.
        let edge = "a".repeat(20);
        assert_eq!(redact(&edge), edge);
    }
}
