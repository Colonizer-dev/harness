//! Jev: the HTTP half of the optional, default-off external classifier. What to ask and what to do
//! with the answer lives in `decide.rs` (issue #582); this file is only the client — the question
//! body, the retry loop, and the defensive parsing of an unverified vendor's reply — plus the routing
//! point's wrapper, [`shadow_opinion`], which the async boot path calls before `routing::decide`
//! runs. The opinion it returns is attached to `routing::Signals` as plain data, so `routing::decide`
//! stays synchronous and pure. The vendor's endpoint, pricing and latency are unverified; treat every
//! number below as a default this integration chose. Either way the safety property holds: with no
//! `JEV_API_KEY` and no setting on, [`shadow_opinion`] makes zero network calls and returns `None` —
//! a misconfigured or nonexistent vendor degrades to "no opinion", never to a blocked boot.

use crate::decide::{Decision, Miss, Mode};
use crate::routing::{JevOpinion, Signals, Tier};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

/// The Jev model this integration is pinned to. Never "jev-latest": a second opinion's thresholds and
/// calibration are specific to one model version and are not assumed to carry over to another.
pub const JEV_MODEL: &str = "jev-1.13.0";

/// The claimed endpoint. Unverified as of this writing — see the module doc.
pub(crate) const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

/// One request's own ceiling, inside a point's overall budget ([`crate::decide::Point`]). A request
/// that runs past it is not retried — the retry loop is only for the rate-limit signals worth waiting
/// out — so a stalled vendor costs one attempt, not the whole budget.
const ATTEMPT_TIMEOUT: Duration = Duration::from_millis(800);

/// A rough, unverified per-input-token price (output is claimed free), used only to log a rough cost
/// estimate alongside each opinion — never metered billing, and never folded into a colony's own
/// routed model cost, since this opinion never picks a model.
pub const JEV_PRICE_PER_MTOK_USD: f64 = 0.042;

/// The three options the routing point offers. A `choice` question returns per-option probabilities
/// plus one confidence for the winner — enough to take as the opinion by argmax, with no hand-picked
/// threshold; thresholds must never be shared across question types or model versions.
pub(crate) const CHOICES: [&str; 3] = ["low", "medium", "high"];

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

/// One raw answer from Jev. `choice` is checked against the point's options by `decide.rs`, not here.
pub(crate) struct Answer {
    pub choice: String,
    pub model: String,
    pub confidence: f64,
    pub estimated_cost_usd: f64,
}

/// Why an ask produced no answer: a timeout (the whole ask, or one attempt) or a genuine error (a
/// non-2xx status, a network failure, a body that would not parse). The point records the difference
/// — a timeout means the vendor was slow, an error means it answered wrong.
#[derive(Debug)]
pub(crate) enum AskError {
    Timeout,
    Error,
}

/// A small HTTP client owned by this feature alone, following the crate's convention of not sharing a
/// `reqwest::Client` across features (see `gateway/mod.rs`'s `Gateway.client`, `mem0.rs`'s `Mem0.client`).
pub struct JevClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl JevClient {
    /// A client against an explicit base URL: the real endpoint from `decide.rs`, or a loopback mock
    /// from a test.
    pub(crate) fn new_at(api_key: String, base_url: String) -> Self {
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

    /// Asks one `choice` question on some context, bounded by `budget`. Never panics or propagates a
    /// boot-failing error: a missing/malformed response, a non-2xx status, a network error or a
    /// timeout each resolves to an [`AskError`].
    pub(crate) async fn ask<C: Serialize>(
        &self,
        question: &str,
        options: &[&str],
        state: &C,
        budget: Duration,
    ) -> Result<Answer, AskError> {
        match tokio::time::timeout(budget, self.ask_inner(question, options, state)).await {
            Ok(result) => result,
            Err(_) => Err(AskError::Timeout),
        }
    }

    async fn ask_inner<C: Serialize>(&self, question: &str, options: &[&str], state: &C) -> Result<Answer, AskError> {
        let serialized = serde_json::to_string(state).map_err(|_| AskError::Error)?;
        let body = json!({
            "model": JEV_MODEL,
            "question": {
                "id": question,
                "type": "choice",
                "options": options,
            },
            "state": state,
        });
        let mut backoff = Duration::from_millis(150);
        // Up to 3 attempts total: the first, and 2 retries on 429/529 with backoff 150ms then 300ms.
        for attempt in 0..3 {
            let response = self
                .client
                .post(&self.base_url)
                .timeout(ATTEMPT_TIMEOUT)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&body)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                // A per-attempt timeout or a network/connect failure: neither is the rate-limit or
                // overload signal that is worth a retry, so this ask is over. A timeout stays a
                // timeout (the vendor was slow); anything else is an error.
                Err(e) => return Err(if e.is_timeout() { AskError::Timeout } else { AskError::Error }),
            };
            let status = response.status();
            if status.is_success() {
                let value: Value = response.json().await.map_err(|_| AskError::Error)?;
                return parse_answer(&value, question, &serialized).ok_or(AskError::Error);
            }
            let retryable = matches!(status.as_u16(), 429 | 529);
            if retryable && attempt < 2 {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }
            return Err(AskError::Error);
        }
        Err(AskError::Error)
    }
}

/// Defensively parses a response: the vendor is unverified, so nothing assumes its exact shape. Looks
/// for a top-level `model` string and an `answers` entry under `question`'s id carrying a `choice`
/// string and a `confidence` number — anything missing or malformed is a failed call, not a panic.
/// The `choice` is returned as written; naming one of the point's options is the point's check.
fn parse_answer(value: &Value, question: &str, request_json: &str) -> Option<Answer> {
    let model = value.get("model")?.as_str()?.to_string();
    let answer = value.get("answers")?.get(question)?;
    let choice = answer.get("choice")?.as_str()?.to_string();
    let confidence = answer.get("confidence")?.as_f64()?;
    // A rough token estimate from the request payload's size (~4 characters per token), times the
    // unverified price above — not metered billing, just enough to keep the cost of asking visible.
    let estimated_tokens = request_json.len() as f64 / 4.0;
    let estimated_cost_usd = estimated_tokens * JEV_PRICE_PER_MTOK_USD / 1_000_000.0;
    Some(Answer {
        choice,
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

/// The routing point's ask, returned to the boot path: the opinion (when the answer parsed to a tier)
/// and the raw [`Result`] and options, so boot can record the ledger row whatever the outcome.
pub struct RoutingAsk {
    /// Jev's tier opinion, or `None` when there was a miss or the pick named no tier.
    pub opinion: Option<JevOpinion>,
    /// The decision layer's answer: a [`Decision`] or the [`Miss`] that explains its absence.
    pub result: Result<Decision, Miss>,
    pub options: &'static [&'static str],
}

/// The entry point the boot path calls before `routing::decide`: the routing point's ask, or `None`
/// when the mode is off (nothing asked, no ledger row). Every real ask delegates to `decide.rs`,
/// which enforces the budget and the off/forbidden/org/key short-circuits; this only builds the
/// condensed state and turns the raw pick back into a tier. A non-tier pick is a clean "no opinion".
pub async fn shadow_opinion(
    mode: Mode,
    org_allows: bool,
    title: &str,
    labels: &[String],
    signals: &Signals,
) -> Option<RoutingAsk> {
    if !mode.asks() {
        return None;
    }
    let state = CondensedState::build(title, labels, signals);
    let result = crate::decide::decide(&crate::decide::ROUTING_TIER, mode, org_allows, &CHOICES, &state).await;
    let opinion = match &result {
        Ok(decision) => Tier::parse(&decision.pick).map(|tier| JevOpinion {
            tier,
            model: decision.model.clone(),
            confidence: decision.confidence,
            estimated_cost_usd: decision.estimated_cost_usd,
        }),
        Err(_) => None,
    };
    Some(RoutingAsk {
        opinion,
        result,
        options: &CHOICES,
    })
}

/// A tiny in-process axum server, shared by this module's tests and `decide.rs`': it answers Jev's
/// shape, counts the requests that reach it, and can be told to fail, stall or answer out of bounds.
#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
    use std::collections::VecDeque;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    /// What the mock answers with: a valid choice at confidence 0.87, a 429 then a valid answer, this
    /// HTTP status every time, no answer at all (well past any point budget), or each of a script's
    /// bodies in turn — for a caller asking several questions, like the brief picker's rounds (#585).
    #[derive(Clone)]
    pub(crate) enum Reply {
        Choice(&'static str),
        Scripted(Arc<Mutex<VecDeque<Value>>>),
        FailOnceThenOk,
        Status(u16),
        NeverRespond,
    }

    #[derive(Clone)]
    struct MockState {
        reply: Reply,
        attempts: Arc<AtomicUsize>,
    }

    fn body(choice: &str) -> Value {
        json!({
            "model": "jev-1.13.0",
            "answers": {"tier": {"choice": choice, "confidence": 0.87}},
        })
    }

    async fn handle(State(state): State<MockState>, Json(_body): Json<Value>) -> axum::response::Response {
        let attempt = state.attempts.fetch_add(1, Ordering::SeqCst);
        match state.reply {
            Reply::Choice(choice) => (axum::http::StatusCode::OK, Json(body(choice))).into_response(),
            Reply::Scripted(script) => match script.lock().unwrap().pop_front() {
                Some(next) => (axum::http::StatusCode::OK, Json(next)).into_response(),
                None => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            },
            Reply::FailOnceThenOk => {
                if attempt == 0 {
                    axum::http::StatusCode::TOO_MANY_REQUESTS.into_response()
                } else {
                    (axum::http::StatusCode::OK, Json(body("high"))).into_response()
                }
            }
            Reply::Status(code) => axum::http::StatusCode::from_u16(code).unwrap().into_response(),
            Reply::NeverRespond => {
                tokio::time::sleep(Duration::from_secs(10)).await;
                axum::http::StatusCode::OK.into_response()
            }
        }
    }

    /// Serves the mock on a loopback port and returns the full URL a client should post to, plus the
    /// counter of requests that reached it.
    pub(crate) async fn serve(reply: Reply) -> (String, Arc<AtomicUsize>) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let state = MockState {
            reply,
            attempts: attempts.clone(),
        };
        let router = Router::new().route("/v1/systemone", post(handle)).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://{addr}/v1/systemone"), attempts)
    }

    /// Serves a mock that answers each of `answers` in turn, then errors once the script runs out.
    pub(crate) async fn serve_scripted(answers: Vec<Value>) -> String {
        serve(Reply::Scripted(Arc::new(Mutex::new(answers.into())))).await.0
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{Reply, serve};
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

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
    async fn a_200_response_with_a_valid_body_returns_the_answer_it_carries() {
        let (base, _) = serve(Reply::Choice("high")).await;
        let client = JevClient::new_at("test-key".into(), base);
        let answer = client
            .ask("tier", &CHOICES, &state(), Duration::from_millis(1800))
            .await
            .expect("expected an answer");
        assert_eq!(answer.choice, "high");
        assert_eq!(answer.model, "jev-1.13.0");
        assert_eq!(answer.confidence, 0.87);
        assert!(answer.estimated_cost_usd > 0.0);
    }

    #[tokio::test]
    async fn a_429_once_then_a_200_still_returns_the_answer_proving_the_retry_worked() {
        let (base, attempts) = serve(Reply::FailOnceThenOk).await;
        let client = JevClient::new_at("test-key".into(), base);
        let answer = client
            .ask("tier", &CHOICES, &state(), Duration::from_millis(1800))
            .await
            .expect("expected the retry to succeed");
        assert_eq!(answer.choice, "high");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_529_every_time_exhausts_the_retries_and_errors_well_under_the_hard_ceiling() {
        let (base, attempts) = serve(Reply::Status(529)).await;
        let client = JevClient::new_at("test-key".into(), base);
        let started = Instant::now();
        let answer = client.ask("tier", &CHOICES, &state(), Duration::from_millis(1800)).await;
        let elapsed = started.elapsed();
        assert!(matches!(answer, Err(AskError::Error)));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        // Backoff alone is 150ms + 300ms = 450ms; loopback requests add very little on top of that,
        // so this proves the retries do not silently run all the way out to the 1.8s hard timeout.
        assert!(elapsed < Duration::from_millis(1500), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn an_endpoint_that_never_responds_times_out_and_does_not_hang() {
        let (base, _) = serve(Reply::NeverRespond).await;
        let client = JevClient::new_at("test-key".into(), base);
        let started = Instant::now();
        let answer = client.ask("tier", &CHOICES, &state(), Duration::from_millis(1800)).await;
        let elapsed = started.elapsed();
        assert!(matches!(answer, Err(AskError::Timeout)));
        // Bounded well below the 1.8s hard ceiling (the 800ms per-attempt timeout fires first), and
        // nowhere near indefinite.
        assert!(elapsed < Duration::from_millis(1800), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn shadow_opinion_with_the_feature_disabled_makes_no_call_and_returns_none() {
        let signals = crate::routing::signals("fix a typo", "short body", &["typo".to_string()], true);
        let ask = shadow_opinion(Mode::Off, true, "fix a typo", &["typo".to_string()], &signals).await;
        assert!(ask.is_none());
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
