//! Deliver through Cratefield (issue #1085): an opt-in second destination for the notifications
//! the mothership already sends to Web Push (push.rs, issue #516). The built-in channel stays the
//! default and is untouched; when the switch here is on, every event [`crate::notify`] hands the
//! push channel — the same event name, the same one line — is also queued and forwarded to the
//! remote-access relay, which fans it out through Cratefield module-notifications to the
//! operator's subscribed browsers and PWAs across all their motherships. The relay already knows
//! which GitHub login each install belongs to, so linking the two is its business; nothing is
//! added on this side.
//!
//! The content rule is #516's, enforced by the same code: the wire notification is
//! [`crate::push::payload`]'s output — title, body, url, tag — plus the event name, a `category`
//! and the queue entry's `id`, and nothing else. One short line about the colony and a link; never
//! question text, agent output or repository content. Unlike the browser push of #742, a
//! question's relayed notification carries no answer token and no option labels — answering
//! happens in a cockpit, not from a card the relay re-sent — so
//! [`crate::push::question_payload`] is deliberately not reused.
//!
//! The relay endpoint contract is remote.rs's signed call (its `signed_call`): headers
//! `x-colonizer-ts` (unix seconds) and `x-colonizer-sig`, Ed25519 over `METHOD\npath\nts\nbody`,
//! against `<relay>/api/installs/<install_id><suffix>`:
//!
//! - `POST /notifications`, body `{"notifications": [...]}` — the queued batch, each notification
//!   carrying the `id` it wore in this install's queue: a flush that times out and retries sends
//!   the same ids again, and the relay delivers an id once, so the at-least-once retries this side
//!   cannot avoid (no relay ack survives the crash between POST and queue write) dedupe there;
//! - `DELETE /notifications` — removes the relay-side subscription(s), as switching off does.
//!
//! Before remote access was ever enabled there is no install id to sign with: switching on is
//! refused (409) and the delivery state reads `no_remote`. Quiet hours, per-category switches and
//! device choice live on the relay — set once, holding for every device, which is the point of the
//! channel — so this side only names the category each event belongs to.

use crate::{
    ApiResult, App, Shared, client_error,
    push_prefs::{self, Prefs},
    util::{short_id, write_private},
};
use anyhow::Result;
use axum::{Json, extract::State, http::Method, http::StatusCode};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path as FsPath, PathBuf},
    sync::{LazyLock, Mutex},
    time::Duration,
};

/// The routes and their rules, registered in `features::ALL`. Everything here is the owner's: the
/// switch decides where the install's notifications travel, and the queue file names colonies.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "cratefield_push",
    routes,
    token_scope: None,
    activity: ACTIVITY,
    kinds: &[],
    start_tasks: Some(start_tasks),
};

fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/push/cratefield", routing::get(get_state).put(put_state))
        .route("/api/push/cratefield/test", routing::post(test))
}

const ACTIVITY: &[crate::activity::Rule] = &[crate::activity::rule(
    "PUT",
    "/api/push/cratefield",
    "settings.save",
    crate::activity::Target::Fixed("the Cratefield delivery", "notifications"),
)];

/// The most queued at once, dropped from when passed.
const MAX_QUEUE: usize = 200;
/// How long a queued notification stays deliverable — the same TTL a Web Push POST declares
/// (push.rs's `TTL: 86400`): a day later it is stale news.
const TTL_SECS: i64 = 24 * 60 * 60;
/// The flush loop's first wait, doubling to [`MAX_WAIT`] while the relay stays unreachable and
/// reset by every delivered batch.
const FIRST_WAIT: Duration = Duration::from_secs(30);
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);

/// The module-notification category the relay fans an event out under. The remote side keeps the
/// switches and the quiet hours, so the finer event name rides along on the wire, but the
/// category is what it groups by.
fn category(event: &str) -> &'static str {
    match event {
        push_prefs::QUESTION => "questions",
        "failed" | "attention" | "needs_rebase" | push_prefs::QUOTA | "provider_degraded" | "judge_degraded" => "failures",
        "pull_request" => "pull_requests",
        push_prefs::DIGEST => "digest",
        "test" => "test",
        _ => "other",
    }
}

/// The switch, `<config_dir>/cratefield-push.json`. No file means off.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    enabled: bool,
    /// RFC3339: when the channel was switched on; cleared when it is switched off.
    #[serde(default)]
    since: Option<String>,
}

/// One queued notification: the wire payload plus the two fields only this side needs — when it
/// was queued (the expiry) and its id (so a flush removes exactly the entries it sent).
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    /// Unix seconds: when it was queued.
    at: i64,
    payload: Value,
}

/// The queue and the delivery bookkeeping, `<config_dir>/cratefield-push-queue.json`: it names
/// colonies, so it is written 0600 and leaves this machine only inside the signed POST.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Queue {
    #[serde(default)]
    entries: Vec<Entry>,
    /// Every notification dropped for capacity or expired unseen — counted, never silent.
    #[serde(default)]
    dropped: u64,
    #[serde(default)]
    last_error: Option<String>,
    /// RFC3339: the last batch the relay accepted.
    #[serde(default)]
    last_delivered: Option<String>,
    /// RFC3339: the last flush attempt, successful or not.
    #[serde(default)]
    last_attempt: Option<String>,
}

fn state_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("cratefield-push.json")
}

fn queue_file(config_dir: &FsPath) -> PathBuf {
    config_dir.join("cratefield-push-queue.json")
}

/// The switch; a missing file is off, and an unparsable one is logged and read as off — a broken
/// channel file must not stop the notifications that still go out.
fn load(config_dir: &FsPath) -> Saved {
    match std::fs::read(state_file(config_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!(
                "cratefield-push: {} could not be parsed ({e}); the channel reads as off",
                state_file(config_dir).display()
            );
            Saved::default()
        }),
        Err(_) => Saved::default(),
    }
}

/// Saves 0600 and atomically: [`write_private`]'s permissions on a fresh temp beside the target,
/// then a rename into place — the pairing chat_images.rs uses — so a reader mid-save sees the
/// whole previous file, never a truncated one (`write_atomic` is async and 0644 on the temp;
/// `enqueue` is sync and these files are private).
fn write_private_atomic(path: &FsPath, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        short_id()
    ));
    if let Err(e) = write_private(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Saves the switch, 0600 and atomic.
fn save(config_dir: &FsPath, saved: &Saved) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;
    write_private_atomic(&state_file(config_dir), &serde_json::to_vec_pretty(saved)?)
}

/// The queue; a missing file is an empty one, and an unparsable one is logged and reset rather
/// than wedging every later save.
fn load_queue(config_dir: &FsPath) -> Queue {
    match std::fs::read(queue_file(config_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!(
                "cratefield-push: {} could not be parsed ({e}); the queue reads as empty",
                queue_file(config_dir).display()
            );
            Queue::default()
        }),
        Err(_) => Queue::default(),
    }
}

/// Saves the queue, 0600 and atomic — it names colonies, so it is written private, and a flush
/// reads it back to remove the delivered batch, so it is never seen half-written.
fn save_queue(config_dir: &FsPath, queue: &Queue) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;
    write_private_atomic(&queue_file(config_dir), &serde_json::to_vec_pretty(queue)?)
}

/// Serialises every read-modify-write of the queue file: an enqueue, a flush and a switch-off can
/// land together. Never an await while it is held.
static STORE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// The notification as it travels: [`crate::push::payload`]'s object — title, body, url, tag, the
/// `colony` key when the event names one, and `silent` as the builder reads it — with the event
/// name and its category added. That builder is only ever handed the one line and the colony id,
/// so the content rule stays one function's to enforce; nothing else about a session rides along.
/// The out-of-quota event keeps its own push's shape instead ([`crate::push::quota_payload`]:
/// the Inbox link and the per-provider tag) when the hook knows the provider.
fn relay_payload(event: &str, text: &str, colony: Option<&str>, provider: Option<&str>) -> Value {
    let mut payload = match (event, provider) {
        (push_prefs::QUOTA, Some(provider)) => crate::push::quota_payload(provider, text, &Prefs::default(), None),
        _ => crate::push::payload(event, text, colony, &Prefs::default(), None),
    };
    let object = payload.as_object_mut().expect("payload is an object");
    object.insert("event".into(), json!(event));
    object.insert("category".into(), json!(category(event)));
    payload
}

/// The wire form of one queued entry: its payload with the entry's `id` on it — the same id the
/// flush removes by, so a retry after a lost answer carries a duplicate the relay can drop.
fn wire(entry: &Entry) -> Value {
    let mut payload = entry.payload.clone();
    payload["id"] = json!(entry.id);
    payload
}

fn is_question(entry: &Entry) -> bool {
    entry.payload["event"].as_str() == Some(push_prefs::QUESTION)
}

/// Applies the queue's two limits in place, answering how many entries were dropped: an entry
/// older than a day expires, and past the cap the oldest non-question goes first — a question is
/// dropped for capacity only when every entry left is one, because it is the one event whose late
/// arrival still matters. Every drop is counted, so nothing disappears silently.
fn enforce(entries: &mut Vec<Entry>, now: i64) -> usize {
    let live = entries.len();
    entries.retain(|entry| now - entry.at <= TTL_SECS);
    let mut dropped = live - entries.len();
    while entries.len() > MAX_QUEUE {
        let oldest = entries.iter().position(|entry| !is_question(entry)).unwrap_or(0);
        entries.remove(oldest);
        dropped += 1;
    }
    dropped
}

/// The notify hook (notify.rs `deliver_routed`): queues one announcement beside the Web Push
/// channel. A cheap no-op while the switch is off — one small file read — and the only thing it
/// is handed is the one line the other channels carry, the colony it names, and the provider for
/// the out-of-quota event.
pub(crate) fn enqueue(app: &App, event: &str, colony: Option<&str>, provider: Option<&str>, text: &str) {
    if !load(&app.cfg.config_dir).enabled {
        return;
    }
    let now = Utc::now().timestamp();
    let entry = Entry {
        id: format!("cfp_{}", short_id()),
        at: now,
        payload: relay_payload(event, text, colony, provider),
    };
    {
        let _store = STORE.lock().expect("the Cratefield queue lock");
        let mut queue = load_queue(&app.cfg.config_dir);
        queue.entries.push(entry);
        queue.dropped += enforce(&mut queue.entries, now) as u64;
        if let Err(e) = save_queue(&app.cfg.config_dir, &queue) {
            tracing::error!( error = %format!("{e:#}"), "cratefield-push: the queue could not be saved ({e:#}); nothing queued" );
            return;
        }
    }
    // The started loop flushes at once on the wake instead of waiting out its backoff; without the
    // loop (a test, mostly) the next hand-driven flush takes the batch.
    WAKE.notify_one();
}

/// Wakes the flush loop the moment something is queued. A permit is stored even with no waiter,
/// so a notify before the loop's next await is never lost.
static WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// One flush at a time: the loop's wake and its own clock can want the same batch, and the loser
/// re-reads the queue it finds rather than re-sending what the winner already delivered.
static FLUSHING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// One flush attempt: delivers the whole queue, or records why not. Answers whether the queue is
/// clear — the loop's backoff resets on a clear and grows on anything else. The switch is
/// re-read under the store before the POST and again before the write-back, so a switch-off
/// (which takes both locks, [`FLUSHING`] around the relay DELETE, [`STORE`] around its wipe) is
/// never raced: no batch goes out for a channel that just went off, and a stale answer never
/// resurrects the queue the disable wiped.
async fn flush(app: &App) -> bool {
    let _flushing = FLUSHING.lock().await;
    let config_dir = &app.cfg.config_dir;
    let now = Utc::now();
    let batch = {
        let _store = STORE.lock().expect("the Cratefield queue lock");
        if !load(config_dir).enabled {
            return true; // off: nothing owed, so the loop's wait stays short for the next switch-on
        }
        let mut queue = load_queue(config_dir);
        let expired = enforce(&mut queue.entries, now.timestamp());
        queue.dropped += expired as u64;
        if expired > 0
            && let Err(e) = save_queue(config_dir, &queue)
        {
            tracing::error!( error = %format!("{e:#}"), "cratefield-push: the queue could not be saved ({e:#}); the expired ones stay counted in memory only" );
        }
        queue.entries.clone()
    };
    if batch.is_empty() {
        return true;
    }
    let body = json!({ "notifications": batch.iter().map(wire).collect::<Vec<_>>() });
    let error = match crate::remote::install_call(app, Method::POST, "/notifications", Some(&body)).await {
        Ok((status, _)) if status.is_success() => None,
        Ok((status, _)) => Some(format!("the relay answered {status}")),
        Err(e) => Some(e.message()),
    };
    let delivered = error.is_none();
    let at = now.to_rfc3339();
    {
        let _store = STORE.lock().expect("the Cratefield queue lock");
        if !load(config_dir).enabled {
            return true; // switched off in flight: the disable wiped the queue; write nothing back
        }
        let mut queue = load_queue(config_dir);
        match error {
            None => {
                // Only the entries this batch carried: one queued while the POST was in flight stays.
                queue.entries.retain(|entry| !batch.iter().any(|sent| sent.id == entry.id));
                queue.last_error = None;
                queue.last_delivered = Some(at.clone());
            }
            Some(why) => queue.last_error = Some(why),
        }
        queue.last_attempt = Some(at);
        if let Err(e) = save_queue(config_dir, &queue) {
            tracing::error!( error = %format!("{e:#}"), "cratefield-push: the queue could not be saved ({e:#})" );
        }
    }
    delivered
}

/// The background flush: every [`FIRST_WAIT`], doubling to [`MAX_WAIT`] while the relay stays
/// unreachable, reset by every delivered batch and by every wake from [`enqueue`].
fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut wait = FIRST_WAIT;
        loop {
            let delivered = tokio::select! {
                () = WAKE.notified() => flush(&app).await,
                () = tokio::time::sleep(wait) => flush(&app).await,
            };
            wait = if delivered { FIRST_WAIT } else { (wait * 2).min(MAX_WAIT) };
        }
    });
}

/// The delivery state the API answers: the switch, and what happened to the queue. `state` reads
/// `no_remote` when remote access was never enabled (no install id, so nothing can be signed),
/// `queued` while a batch waits for its first attempt, and `unreachable` from the first failed
/// one until a batch goes through and clears the error.
async fn view(app: &App) -> Value {
    let (saved, queue) = {
        // One consistent read of both files: an enqueue or a flush between the two reads would
        // answer a queue that never existed.
        let _store = STORE.lock().expect("the Cratefield queue lock");
        (load(&app.cfg.config_dir), load_queue(&app.cfg.config_dir))
    };
    let linked = app.remote.install_id().await.is_some();
    let state = if !saved.enabled {
        "off"
    } else if !linked {
        "no_remote"
    } else if !queue.entries.is_empty() {
        if queue.last_error.is_some() { "unreachable" } else { "queued" }
    } else {
        "ok"
    };
    json!({
        "enabled": saved.enabled,
        "since": saved.since,
        "state": state,
        "queued": queue.entries.len(),
        "dropped": queue.dropped,
        "last_error": queue.last_error,
        "last_delivered": queue.last_delivered,
        "last_attempt": queue.last_attempt,
    })
}

/// `GET /api/push/cratefield`: the switch and the queue's bookkeeping.
pub async fn get_state(State(app): State<Shared>) -> ApiResult<Value> {
    Ok(Json(view(&app).await))
}

#[derive(Deserialize)]
pub struct SetRequest {
    enabled: bool,
}

/// `PUT /api/push/cratefield {"enabled": …}`. Switching on requires the remote-access
/// registration (409 without it): the relay endpoint is an install endpoint, and there is nothing
/// to sign with before the first enable. Switching off clears the queue and tells the relay to
/// drop its subscription(s); a relay that cannot be told still leaves the channel off here, with
/// its refusal in the state's `last_error` so the cleanup is not silent. Both writes happen under
/// the store, and the off path under the flush lock too — around the relay DELETE — so no flush
/// POSTs after the relay was told to forget this install, and no enqueue or stale write-back
/// resurrects the wiped queue.
pub async fn put_state(State(app): State<Shared>, Json(body): Json<SetRequest>) -> ApiResult<Value> {
    if body.enabled == load(&app.cfg.config_dir).enabled {
        return Ok(Json(view(&app).await)); // no change: nothing to do
    }
    if body.enabled {
        if app.remote.install_id().await.is_none() {
            return Err(client_error(
                StatusCode::CONFLICT,
                "remote access has no link yet; switch remote access on first",
            ));
        }
        {
            let _store = STORE.lock().expect("the Cratefield queue lock");
            save(
                &app.cfg.config_dir,
                &Saved {
                    enabled: true,
                    since: Some(Utc::now().to_rfc3339()),
                },
            )?;
        }
    } else {
        let _flushing = FLUSHING.lock().await;
        let refused = match app.remote.install_id().await {
            None => None, // never registered: the relay holds no subscription to remove
            Some(_) => match crate::remote::install_call(&app, Method::DELETE, "/notifications", None).await {
                Ok((status, _)) if status.is_success() => None,
                Ok((status, _)) => Some(format!("the relay answered {status}")),
                Err(e) => Some(e.message()),
            },
        };
        {
            let _store = STORE.lock().expect("the Cratefield queue lock");
            save(
                &app.cfg.config_dir,
                &Saved {
                    enabled: false,
                    since: None,
                },
            )?;
            // The queue goes with the channel: entries, counters and the old bookkeeping, with the
            // relay's refusal — if there was one — written back in so it is still on the record.
            save_queue(
                &app.cfg.config_dir,
                &Queue {
                    last_error: refused,
                    ..Queue::default()
                },
            )?;
        }
    }
    Ok(Json(view(&app).await))
}

/// The test notification's one line: the machine it came from, or the product when the hostname
/// is unreadable. It is not about any colony, so it names none.
fn test_line() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match host {
        Some(host) => format!("Test notification from {host}"),
        None => "Test notification from Colonizer".into(),
    }
}

/// `POST /api/push/cratefield/test`: one test notification through the whole pipe — queued like a
/// real event, then flushed at once — and the state back.
pub async fn test(State(app): State<Shared>) -> ApiResult<Value> {
    if !load(&app.cfg.config_dir).enabled {
        return Err(client_error(
            StatusCode::CONFLICT,
            "Deliver through Cratefield is off; switch it on first",
        ));
    }
    enqueue(&app, "test", None, None, &test_line());
    flush(&app).await;
    Ok(Json(view(&app).await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::Ed25519KeyPair;
    use std::sync::Arc;

    /// A colony as `sessions.json` holds one, with a sentinel in every free-text field: none of it
    /// may reach a relayed notification, because [`relay_payload`] is only ever handed the one
    /// line and the id. The same fixture push.rs's payload tests pin.
    fn colony() -> crate::sessions::Session {
        serde_json::from_value(json!({
            "id": "abc123",
            "repo": "acme/webshop",
            "org": "acme",
            "issue": 42,
            "issue_title": "SENTINEL-issue-title",
            "instructions": "fix the bug. sk-ant-api03-SENTINEL",
            "status": "waiting_for_answer",
            "branch": "colonizer/SENTINEL-branch",
            "worktree": "/colonizer/worktrees/wt",
            "sandbox": "colonizer-abc123",
            "agent": "claude",
            "pr_url": "https://github.com/acme/webshop/pull/7",
            "error": "SENTINEL-error",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    fn entry_at(at: i64, event: &str) -> Entry {
        Entry {
            id: format!("cfp_{}", short_id()),
            at,
            payload: relay_payload(event, "acme/webshop #42 failed", None, None),
        }
    }

    #[test]
    fn every_push_event_maps_to_its_relay_category() {
        for (event, want) in [
            (push_prefs::QUESTION, "questions"),
            ("failed", "failures"),
            ("attention", "failures"),
            ("needs_rebase", "failures"),
            (push_prefs::QUOTA, "failures"),
            ("provider_degraded", "failures"),
            ("pull_request", "pull_requests"),
            (push_prefs::DIGEST, "digest"),
            ("test", "test"),
        ] {
            assert_eq!(category(event), want, "{event}");
        }
    }

    /// The mapping stays in step with the events a device can switch: none of them may land in
    /// `other`, or the relay would file a live event nowhere.
    #[test]
    fn no_event_the_devices_can_switch_lands_in_other() {
        for (name, _) in push_prefs::EVENTS {
            assert_ne!(category(name), "other", "{name}");
        }
    }

    /// The relayed question carries the one line and the labels' absence is the point: no answer
    /// key at all, so no token and no choices ever leave on this channel.
    #[test]
    fn the_relay_payload_carries_the_one_line_and_nothing_of_the_session() {
        let session = colony();
        let text = crate::notify::Event::Question.text(&session.repo, session.issue);
        let value = relay_payload("question", &text, Some(&session.id), None);
        let body = serde_json::to_string(&value).unwrap();
        for sentinel in [
            "SENTINEL-issue-title",
            "SENTINEL-branch",
            "SENTINEL-error",
            "sk-ant-api03-SENTINEL",
        ] {
            assert!(!body.contains(sentinel), "the relay payload leaked {sentinel}: {body}");
        }
        assert!(
            value.get("answer").is_none(),
            "no answer key: answering is not a relayed card's job"
        );
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["body", "category", "colony", "event", "silent", "tag", "title", "url"]);
        assert_eq!(value["title"], "Colony asks a question");
        assert_eq!(value["body"], "acme/webshop #42 needs an answer");
        assert_eq!(value["event"], "question");
        assert_eq!(value["category"], "questions");
        assert_eq!(value["url"], "/?colony=abc123");
    }

    /// The out-of-quota notification is the push's own card (push.rs `quota_payload`): the Inbox
    /// deep link and the per-provider tag, filed under failures like every other bad news.
    #[test]
    fn the_quota_notification_reuses_the_quota_pushs_shape() {
        let value = relay_payload(push_prefs::QUOTA, "anthropic is out of quota", None, Some("anthropic"));
        assert_eq!(value["url"], crate::push::QUOTA_URL);
        assert_eq!(value["tag"], "quota-anthropic");
        assert_eq!(value["event"], push_prefs::QUOTA);
        assert_eq!(value["category"], "failures");
        assert_eq!(value.get("colony"), None, "the card is about many colonies");
        // Without the provider the hook falls back to the generic shape rather than guessing one.
        let generic = relay_payload(push_prefs::QUOTA, "a provider is out of quota", None, None);
        assert_eq!(generic["url"], "/");
        assert_eq!(generic["tag"], push_prefs::QUOTA);
    }

    #[test]
    fn the_queue_drops_the_oldest_non_question_first_and_questions_only_as_a_last_resort() {
        let now = 1_000_000;
        let mut entries: Vec<Entry> = (0..MAX_QUEUE).map(|_| entry_at(now, "failed")).collect();
        entries.push(entry_at(now, "question"));
        assert_eq!(enforce(&mut entries, now), 1, "the one over the cap went");
        assert_eq!(entries.len(), MAX_QUEUE);
        assert!(is_question(entries.last().unwrap()), "the question survived the overflow");
        // Nothing but questions left: the oldest goes, counted like any other drop.
        let mut questions: Vec<Entry> = (0..MAX_QUEUE + 3).map(|_| entry_at(now, "question")).collect();
        assert_eq!(enforce(&mut questions, now), 3);
        assert!(questions.iter().all(is_question));
    }

    #[test]
    fn expired_entries_expire_and_are_counted_as_dropped() {
        let now = 1_000_000;
        let mut entries = vec![entry_at(now - TTL_SECS - 1, "failed"), entry_at(now - 1, "question")];
        assert_eq!(enforce(&mut entries, now), 1, "the day-old one went");
        assert_eq!(entries.len(), 1);
        assert!(is_question(&entries[0]));
    }

    /// An App with the channel switched on and an install identity to sign with, as an enable left
    /// it; the test points `app.remote` at its own stand-in relay and registers the install id.
    fn switched_on(root: &FsPath) -> crate::Shared {
        let app = crate::tests::test_app(root);
        std::fs::create_dir_all(app.cfg.config_dir.join("remote")).unwrap();
        let key = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        write_private(&app.cfg.config_dir.join("remote/key"), key.as_ref()).unwrap();
        save(
            &app.cfg.config_dir,
            &Saved {
                enabled: true,
                since: None,
            },
        )
        .unwrap();
        app
    }

    /// The stand-in relay: records each POST's path, headers and body, answers 204.
    type Captures = Arc<std::sync::Mutex<Vec<(String, axum::http::HeaderMap, Value)>>>;

    async fn relay_server(captures: Captures) -> std::net::SocketAddr {
        let router = axum::Router::new().route(
            "/api/installs/{id}/notifications",
            axum::routing::post(
                move |axum::extract::Path(id): axum::extract::Path<String>,
                      headers: axum::http::HeaderMap,
                      body: axum::body::Bytes| {
                    let captures = captures.clone();
                    async move {
                        captures.lock().unwrap().push((
                            format!("/api/installs/{id}/notifications"),
                            headers,
                            serde_json::from_slice(&body).unwrap_or(Value::Null),
                        ));
                        axum::http::StatusCode::NO_CONTENT
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        addr
    }

    /// The whole path, against a stand-in relay that answers 204: the signed POST lands on the
    /// install's notifications endpoint with the entry's id on the wire, the batch empties the
    /// queue and stamps the delivery.
    #[tokio::test]
    async fn a_flush_delivers_the_batch_through_the_signed_call_and_clears_the_queue() {
        let captures: Captures = Arc::default();
        let addr = relay_server(captures.clone()).await;
        let root = std::env::temp_dir().join(format!("colonizer-cratefield-{}", short_id()));
        let app = switched_on(&root);
        app.remote.set_relay(format!("ws://{addr}")).await;
        app.remote.set_install_id("inst_cfp01").await;
        enqueue(&app, "failed", Some("abc123"), None, "acme/webshop #42 failed");
        let id = {
            let _store = STORE.lock().expect("the Cratefield queue lock");
            load_queue(&app.cfg.config_dir).entries[0].id.clone()
        };
        assert!(flush(&app).await);
        {
            let captures = captures.lock().unwrap();
            let (path, headers, body) = &captures[0];
            assert_eq!(path, "/api/installs/inst_cfp01/notifications");
            assert!(
                headers.contains_key("x-colonizer-ts") && headers.contains_key("x-colonizer-sig"),
                "the relay call is signed: {headers:?}"
            );
            assert_eq!(body["notifications"][0]["id"], id, "the id rides for the relay's dedupe");
            assert_eq!(body["notifications"][0]["event"], "failed");
            assert_eq!(body["notifications"][0]["category"], "failures");
            assert_eq!(body["notifications"][0]["body"], "acme/webshop #42 failed");
        }
        let queue = load_queue(&app.cfg.config_dir);
        assert!(queue.entries.is_empty(), "a delivered batch leaves nothing queued");
        assert!(queue.last_error.is_none());
        assert!(queue.last_delivered.is_some());
        assert!(queue.last_attempt.is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each state the queue can read as, in one walk: off, on with no link (`no_remote`), a batch
    /// waiting (`queued`), a relay that cannot be reached (`unreachable`, batch kept and error
    /// recorded), and the delivered calm after (`ok`).
    #[tokio::test]
    async fn the_delivery_state_names_what_happened_to_the_queue() {
        let root = std::env::temp_dir().join(format!("colonizer-cratefield-{}", short_id()));
        let app = crate::tests::test_app(&root);
        std::fs::create_dir_all(app.cfg.config_dir.join("remote")).unwrap();
        assert_eq!(view(&app).await["state"], "off");
        save(
            &app.cfg.config_dir,
            &Saved {
                enabled: true,
                since: None,
            },
        )
        .unwrap();
        assert_eq!(
            view(&app).await["state"],
            "no_remote",
            "never registered: nothing to sign with"
        );
        let key = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        write_private(&app.cfg.config_dir.join("remote/key"), key.as_ref()).unwrap();
        app.remote.set_install_id("inst_cfp02").await;
        app.remote.set_relay("ws://127.0.0.1:1".into()).await; // a port with no listener
        enqueue(&app, "question", Some("abc123"), None, "acme/webshop #42 needs an answer");
        assert_eq!(view(&app).await["state"], "queued");
        assert!(!flush(&app).await, "an unreachable relay is not a delivery");
        let queue = load_queue(&app.cfg.config_dir);
        assert_eq!(queue.entries.len(), 1, "the batch stays queued for the next attempt");
        assert!(queue.last_error.is_some());
        assert_eq!(view(&app).await["state"], "unreachable");
        let captures: Captures = Arc::default();
        let addr = relay_server(captures).await;
        app.remote.set_relay(format!("ws://{addr}")).await;
        assert!(flush(&app).await);
        assert_eq!(view(&app).await["state"], "ok");
        let _ = std::fs::remove_dir_all(&root);
    }
}
