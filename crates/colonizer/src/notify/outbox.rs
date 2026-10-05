//! Webhook retries and the dead letter (issue #898).
//!
//! A webhook POST that fails — a transport error or a non-2xx answer — is not lost: it goes into an
//! outbox and is tried again with exponential backoff and jitter, at most [`MAX_ATTEMPTS`] times in
//! all. One that still fails is moved to the dead letter, where it stays until the owner replays or
//! discards it. Both live in one file under the data directory, so a restart neither forgets a
//! retry nor loses a dead letter.
//!
//! Every attempt sends the same body — so the same event id (issue #896) — re-signed with the
//! current secret and a fresh timestamp, and a receiver that dedupes on the id sees the event once
//! however many attempts it took. What is kept is what was sent: the payload, which carries no
//! repository content and never a secret, and the address it was sent to. The signing secret is
//! never written here; it is read where it is used, as the first attempt reads it.

use super::{post, secret};
use crate::{
    ApiResult, App, Shared, client_error,
    util::{short_id, write_private},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::LazyLock, time::Duration};
use tokio::sync::Mutex;

/// The most attempts one delivery gets, the first included: with the backoff below, the last retry
/// comes roughly fifteen minutes after the first failure.
pub const MAX_ATTEMPTS: u32 = 6;
/// The delay before the first retry; each later one doubles it.
const BASE_DELAY_SECS: f64 = 30.0;
/// No retry waits longer than this, however many attempts came before.
const MAX_DELAY_SECS: f64 = 3600.0;
/// How far a delay is moved either way, as a fraction, so a burst of failures does not retry in
/// lockstep against a receiver that is just coming back.
const JITTER: f64 = 0.2;
/// The most dead letters kept: past it the oldest goes, so a receiver that is gone for good cannot
/// grow the file forever.
const DEAD_LETTER_CAP: usize = 500;
/// The most deliveries waiting for a retry: past it the oldest is dead-lettered early.
const PENDING_CAP: usize = 1000;
/// How often the retry worker looks for deliveries that are due.
const TICK: Duration = Duration::from_secs(5);

/// The outbox file's one writer at a time: every change is a read, a change and a write of the
/// whole file, and two at once would lose one of them.
static STORE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Who a delivery is for. The owner's webhook is the notify module's `webhook_url`.
pub const OWNER: &str = "owner";

/// One webhook delivery as the outbox keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Delivery {
    /// The event id and the target: one event to one receiver.
    pub key: String,
    pub event_id: String,
    pub event: String,
    /// [`OWNER`], or the subscription the delivery is for.
    pub target: String,
    pub url: String,
    /// The exact JSON sent, every attempt.
    pub body: String,
    /// The colony the event is about, so the last failure can be logged where its person looks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colony: Option<String>,
    pub attempts: u32,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    /// When the next attempt is due; `None` once dead-lettered.
    #[serde(default)]
    pub next_at: Option<DateTime<Utc>>,
    pub last_error: String,
}

impl Delivery {
    /// A delivery about to be tried for the first time.
    pub fn new(target: &str, url: &str, payload: &Value, body: String, colony: Option<String>) -> Self {
        let event_id = payload["id"].as_str().unwrap_or_default().to_string();
        let now = Utc::now();
        Self {
            key: format!("{event_id}.{target}"),
            event_id,
            event: payload["event"].as_str().unwrap_or_default().to_string(),
            target: target.to_string(),
            url: url.to_string(),
            body,
            colony,
            attempts: 0,
            first_at: now,
            last_at: now,
            next_at: None,
            last_error: String::new(),
        }
    }

    /// What the API shows of a delivery: everything but the body, and the address without its
    /// query string, where a webhook URL sometimes keeps a token.
    fn view(&self) -> Value {
        json!({
            "key": self.key,
            "event_id": self.event_id,
            "event": self.event,
            "target": self.target,
            "url": self.url.split(['?', '#']).next().unwrap_or_default(),
            "colony": self.colony,
            "attempts": self.attempts,
            "first_at": self.first_at,
            "last_at": self.last_at,
            "next_at": self.next_at,
            "last_error": self.last_error,
        })
    }
}

/// The outbox file: deliveries waiting for a retry, the dead letter, and the last success.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    pub pending: Vec<Delivery>,
    #[serde(default)]
    pub dead: Vec<Delivery>,
    #[serde(default)]
    pub last_success_at: Option<DateTime<Utc>>,
}

fn file(app: &App) -> PathBuf {
    app.cfg.data_dir.join("notify-webhook-outbox.json")
}

/// The outbox as saved. A missing file is an empty outbox; an unreadable one is logged and treated
/// as empty rather than stopping notifications.
pub fn load(app: &App) -> Book {
    let path = file(app);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!(
                "notify: {} could not be parsed ({e}); starting an empty outbox",
                path.display()
            );
            Book::default()
        }),
        Err(_) => Book::default(),
    }
}

/// Saves the outbox: written beside the target and renamed into place, so a crash never leaves
/// half a file, and mode 0600, since a webhook address can carry a token.
fn save(app: &App, book: &Book) {
    let path = file(app);
    let tmp = path.with_file_name(format!("notify-webhook-outbox.json.{}.tmp", short_id()));
    let result = serde_json::to_vec_pretty(book)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            write_private(&tmp, &bytes)
        })
        .and_then(|()| std::fs::rename(&tmp, &path).map_err(anyhow::Error::from));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("notify: could not save the webhook outbox ({e:#})");
    }
}

/// The delay before attempt `attempts + 1`, after `attempts` failures: 30 s doubled per failure,
/// capped at an hour, moved by `jitter` (a fraction in `-1.0..=1.0`, scaled to ±[`JITTER`]).
pub fn backoff(attempts: u32, jitter: f64) -> ChronoDuration {
    let exponent = attempts.saturating_sub(1).min(20) as i32;
    let base = (BASE_DELAY_SECS * 2f64.powi(exponent)).min(MAX_DELAY_SECS);
    let secs = base * (1.0 + JITTER * jitter.clamp(-1.0, 1.0));
    ChronoDuration::milliseconds((secs * 1000.0) as i64)
}

/// A random jitter in `-1.0..=1.0`.
fn jitter() -> f64 {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 2];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        return 0.0;
    }
    f64::from(u16::from_le_bytes(bytes)) / f64::from(u16::MAX) * 2.0 - 1.0
}

/// The signing secret for a target, read where it is used.
fn signing_secret(app: &App, target: &str) -> Option<String> {
    match target {
        OWNER => secret(app).map(|(value, _)| value),
        _ => None,
    }
}

/// One attempt at a delivery: the same body, signed afresh.
async fn attempt(app: &App, client: &reqwest::Client, delivery: &Delivery) -> anyhow::Result<()> {
    let signing = signing_secret(app, &delivery.target);
    post(client, &delivery.url, signing.as_deref(), &delivery.event_id, &delivery.body).await
}

/// What became of a failed attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failed {
    /// It will be tried again.
    Retrying { attempt: u32 },
    /// It ran out of attempts and is in the dead letter now.
    DeadLettered,
}

/// Records a failed attempt in `book`: back into the pending list with its next time, or — past
/// [`MAX_ATTEMPTS`] — into the dead letter.
fn record_failure(book: &mut Book, mut delivery: Delivery, error: String, now: DateTime<Utc>, jitter: f64) -> Failed {
    delivery.attempts += 1;
    delivery.last_at = now;
    delivery.last_error = crate::util::truncate(&error, 300);
    book.pending.retain(|d| d.key != delivery.key);
    if delivery.attempts >= MAX_ATTEMPTS {
        delivery.next_at = None;
        dead_letter(book, delivery);
        return Failed::DeadLettered;
    }
    let attempt = delivery.attempts;
    delivery.next_at = Some(now + backoff(delivery.attempts, jitter));
    book.pending.push(delivery);
    if book.pending.len() > PENDING_CAP {
        let mut oldest = book.pending.remove(0);
        oldest.next_at = None;
        oldest.last_error = format!("{} (dead-lettered early: too many deliveries waiting)", oldest.last_error);
        dead_letter(book, oldest);
    }
    Failed::Retrying { attempt }
}

fn dead_letter(book: &mut Book, delivery: Delivery) {
    book.dead.retain(|d| d.key != delivery.key);
    book.dead.push(delivery);
    if book.dead.len() > DEAD_LETTER_CAP {
        let excess = book.dead.len() - DEAD_LETTER_CAP;
        book.dead.drain(..excess);
    }
}

/// The first attempt at a delivery, made where the event is announced. `Ok(())` when the receiver
/// took it; otherwise the delivery is in the outbox for a retry (or, with one attempt allowed,
/// already dead) and the answer says which.
pub async fn send(app: &App, client: &reqwest::Client, delivery: Delivery) -> Result<(), (String, Failed)> {
    match attempt(app, client, &delivery).await {
        Ok(()) => {
            let _guard = STORE.lock().await;
            let mut book = load(app);
            book.last_success_at = Some(Utc::now());
            save(app, &book);
            Ok(())
        }
        Err(e) => {
            let error = format!("{e:#}");
            let _guard = STORE.lock().await;
            let mut book = load(app);
            let failed = record_failure(&mut book, delivery, error.clone(), Utc::now(), jitter());
            save(app, &book);
            Err((error, failed))
        }
    }
}

/// Tries every delivery due by `now` once, and answers how many the receivers took. The worker
/// calls it every few seconds; a test calls it with a `now` in the future to skip the waiting.
pub async fn retry_due(app: &App, client: &reqwest::Client, now: DateTime<Utc>) -> usize {
    let due: Vec<Delivery> = {
        let _guard = STORE.lock().await;
        load(app)
            .pending
            .into_iter()
            .filter(|d| d.next_at.is_none_or(|at| at <= now))
            .collect()
    };
    let mut delivered = 0;
    for delivery in due {
        let outcome = attempt(app, client, &delivery).await;
        let _guard = STORE.lock().await;
        let mut book = load(app);
        // Replayed, discarded or already delivered while this attempt was in flight: leave it be.
        if !book.pending.iter().any(|d| d.key == delivery.key) {
            continue;
        }
        match outcome {
            Ok(()) => {
                book.pending.retain(|d| d.key != delivery.key);
                book.last_success_at = Some(Utc::now());
                delivered += 1;
                save(app, &book);
            }
            Err(e) => {
                let colony = delivery.colony.clone();
                let event = delivery.event.clone();
                let failed = record_failure(&mut book, delivery, format!("{e:#}"), now, jitter());
                save(app, &book);
                drop(_guard);
                if failed == Failed::DeadLettered {
                    let line = format!(
                        "notify: the webhook gave up on the {event} event after {MAX_ATTEMPTS} attempts ({e:#}); it is in the dead letter, where Settings can replay it"
                    );
                    match colony {
                        Some(id) => app.session_log_as(crate::protocol::Origin::Notify, &id, "warn", line).await,
                        None => eprintln!("{line}"),
                    }
                }
            }
        }
    }
    delivered
}

/// Runs the retry worker forever.
pub(crate) async fn run(app: Shared) {
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
    else {
        eprintln!("notify: could not build an HTTP client; webhook retries are off");
        return;
    };
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        retry_due(&app, &client, Utc::now()).await;
    }
}

/// Replays one dead letter now: one attempt, and the answer. A replay that fails goes back into the
/// dead letter with its attempt counted, rather than starting a fresh round of retries the owner
/// did not ask for. `None` when no dead letter has that key.
pub async fn replay(app: &App, client: &reqwest::Client, key: &str) -> Option<Result<(), String>> {
    let delivery = {
        let _guard = STORE.lock().await;
        let book = load(app);
        book.dead.into_iter().find(|d| d.key == key)?
    };
    let outcome = attempt(app, client, &delivery).await.map_err(|e| format!("{e:#}"));
    let _guard = STORE.lock().await;
    let mut book = load(app);
    match &outcome {
        Ok(()) => {
            book.dead.retain(|d| d.key != key);
            book.last_success_at = Some(Utc::now());
        }
        Err(error) => {
            if let Some(dead) = book.dead.iter_mut().find(|d| d.key == key) {
                dead.attempts += 1;
                dead.last_at = Utc::now();
                dead.last_error = crate::util::truncate(error, 300);
            }
        }
    }
    save(app, &book);
    Some(outcome)
}

// ---------------------------------------------------------------------------
// The owner's API
// ---------------------------------------------------------------------------

/// `GET /api/notify/deliveries`: the deliveries waiting for a retry, the dead letter and the last
/// success — what Settings shows next to the webhook.
pub async fn deliveries(State(app): State<Shared>) -> Json<Value> {
    let book = {
        let _guard = STORE.lock().await;
        load(&app)
    };
    Json(json!({
        "pending": book.pending.iter().map(Delivery::view).collect::<Vec<_>>(),
        "dead_letters": book.dead.iter().rev().map(Delivery::view).collect::<Vec<_>>(),
        "last_success_at": book.last_success_at,
        "max_attempts": MAX_ATTEMPTS,
    }))
}

fn client() -> Result<reqwest::Client, crate::AppError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| client_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("no HTTP client: {e}")))
}

/// `POST /api/notify/dead-letters/{key}/replay`: one attempt now. 200 with `{delivered, error}`
/// either way — a receiver still failing is an answer, not a fault — and 404 for an unknown key.
pub async fn replay_one(State(app): State<Shared>, Path(key): Path<String>) -> ApiResult<Value> {
    match replay(&app, &client()?, &key).await {
        None => Err(client_error(StatusCode::NOT_FOUND, "no dead letter has that key")),
        Some(Ok(())) => Ok(Json(json!({"delivered": true, "error": null}))),
        Some(Err(error)) => Ok(Json(json!({"delivered": false, "error": error}))),
    }
}

/// `POST /api/notify/dead-letters/replay`: every dead letter, oldest first, one attempt each.
pub async fn replay_all(State(app): State<Shared>) -> ApiResult<Value> {
    let client = client()?;
    let keys: Vec<String> = {
        let _guard = STORE.lock().await;
        load(&app).dead.into_iter().map(|d| d.key).collect()
    };
    let (mut delivered, mut failed) = (0, 0);
    for key in keys {
        match replay(&app, &client, &key).await {
            Some(Ok(())) => delivered += 1,
            Some(Err(_)) => failed += 1,
            None => {}
        }
    }
    Ok(Json(json!({"delivered": delivered, "failed": failed})))
}

/// `DELETE /api/notify/dead-letters/{key}`: discards one dead letter. 404 for an unknown key.
pub async fn discard(State(app): State<Shared>, Path(key): Path<String>) -> ApiResult<Value> {
    let _guard = STORE.lock().await;
    let mut book = load(&app);
    let before = book.dead.len();
    book.dead.retain(|d| d.key != key);
    if book.dead.len() == before {
        return Err(client_error(StatusCode::NOT_FOUND, "no dead letter has that key"));
    }
    save(&app, &book);
    Ok(Json(json!({"discarded": key})))
}
