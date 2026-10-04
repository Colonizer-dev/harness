//! Draining the mothership ahead of an update or a restart (issue #880).
//!
//! An update restarts the process in place (`update.rs`). A colony still booting (`Starting`) when
//! that happens dies with the old process: the boot it was in the middle of fails against a
//! microVM the new process is already bringing up under the same sandbox name, and `recover` used
//! to strand it `Stopped`. Draining closes the window from the near side: while the flag is set the
//! queue admits nothing new, so every boot either finishes or never starts, and the update waits
//! for the in-flight colonies (`Starting`, `Publishing`) to leave those states before it installs
//! and restarts. Anything still in flight when the wait gives up is the caller's call: the update
//! refuses if a publish is among them (a restart would cut the push off), and otherwise installs and
//! leaves an interrupted boot to `recover`, which now requeues it instead of stopping it; SIGTERM
//! goes on either way, having no way to refuse.
//!
//! The flag is process state, held on the `App` like every other module's: a restart drops it,
//! which is the right default — a mothership that has just come back is not draining.

use crate::{Shared, sessions::SessionStatus};
use axum::{Json, body::Bytes, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// How long a drain gets before the caller goes on without it: long enough for a boot or a publish
/// to finish, short enough that a wedged one does not hold an update or a SIGTERM for ever.
/// Overridable with `COLONIZER_DRAIN_TIMEOUT_SECS`.
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How often the wait re-reads the colony list.
const POLL: Duration = Duration::from_secs(1);

/// The drain flag and when it was set.
#[derive(Default)]
pub struct Drain {
    draining: AtomicBool,
    since: Mutex<Option<DateTime<Utc>>>,
}

impl Drain {
    /// Enters draining, stamping `since` the first time; a second call while draining leaves the
    /// stamp where it is.
    pub fn enter(&self) {
        self.draining.store(true, Ordering::SeqCst);
        let mut since = self.since.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if since.is_none() {
            *since = Some(Utc::now());
        }
    }

    /// Stops draining: the queue admits again on its next tick.
    pub fn clear(&self) {
        self.draining.store(false, Ordering::SeqCst);
        *self.since.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    pub fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    pub fn since(&self) -> Option<DateTime<Utc>> {
        *self.since.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The wait's budget, from `COLONIZER_DRAIN_TIMEOUT_SECS` or the default. A missing or unparsable
/// value means the default; a parsed one wins.
pub fn timeout() -> Duration {
    parse_timeout(crate::util::env_nonempty("COLONIZER_DRAIN_TIMEOUT_SECS"))
}

fn parse_timeout(raw: Option<String>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_DRAIN_TIMEOUT)
}

/// How many colonies a restart would interrupt right now: the ones booting or publishing, the two
/// statuses `recover` and the update both judge.
pub async fn in_flight(app: &Shared) -> usize {
    app.sessions
        .read()
        .await
        .iter()
        .filter(|s| matches!(s.status, SessionStatus::Starting | SessionStatus::Publishing))
        .count()
}

/// Enters draining and waits until nothing is booting or publishing, or until `timeout` runs out.
/// `true` when the queue is quiet, `false` on the timeout — the caller decides whether to go on
/// anyway (the update installs only if no publish is left, and requeues any boot the wait gave up
/// on; SIGTERM exits either way, having no way to refuse).
pub async fn drain_and_wait(app: &Shared, timeout: Duration) -> bool {
    app.drain.enter();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut announced = false;
    loop {
        let waiting = in_flight(app).await;
        if waiting == 0 {
            return true;
        }
        if !announced {
            eprintln!("drain: waiting for {waiting} colony(ies) to finish booting or publishing (up to {timeout:?})");
            announced = true;
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!("drain: {waiting} colony(ies) still in flight after {timeout:?}; going on without them");
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/* ------------------------------------------------------------------ routes */

/// The optional body of `POST /api/admin/drain`: absent, blank or `{}` all mean "start draining",
/// and `{"draining": false}` cancels it.
#[derive(Default, Deserialize)]
struct DrainRequest {
    draining: Option<bool>,
}

/// `GET /api/admin/drain` — the drain state and whether the queue has gone quiet.
pub async fn get(State(app): State<Shared>) -> crate::ApiResult<Value> {
    Ok(Json(snapshot(&app).await))
}

/// `POST /api/admin/drain` — start draining (the default), or cancel it with `{"draining": false}`.
/// Owner-only: the route is not listed in `api_tokens::classify`, so a scoped token cannot reach it.
pub async fn post(State(app): State<Shared>, body: Bytes) -> crate::ApiResult<Value> {
    // No body is the plain "start draining" call, like the update button sending none.
    let draining = if body.iter().all(|b| b.is_ascii_whitespace()) {
        true
    } else {
        match serde_json::from_slice::<DrainRequest>(&body) {
            Ok(request) => request.draining.unwrap_or(true),
            Err(e) => {
                return Err(crate::client_error(
                    StatusCode::BAD_REQUEST,
                    &format!("the drain body must be JSON like {{\"draining\": false}}: {e}"),
                ));
            }
        }
    };
    if draining {
        app.drain.enter();
    } else {
        app.drain.clear();
    }
    Ok(Json(snapshot(&app).await))
}

/// What both routes answer with, and what `/api/status` reads its `draining` key from.
async fn snapshot(app: &Shared) -> Value {
    let in_flight = in_flight(app).await;
    let draining = app.drain.draining();
    json!({
        "draining": draining,
        "since": app.drain.since(),
        "in_flight": in_flight,
        "ready": draining && in_flight == 0,
    })
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/admin/drain", routing::get(get).post(post))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;
    use crate::tests::{temp_root, test_app};

    #[test]
    fn a_timeout_is_read_from_the_environment_or_defaulted() {
        assert_eq!(parse_timeout(None), DEFAULT_DRAIN_TIMEOUT);
        assert_eq!(parse_timeout(Some("not a number".into())), DEFAULT_DRAIN_TIMEOUT);
        assert_eq!(parse_timeout(Some(" 15 ".into())), Duration::from_secs(15));
    }

    /// Entering is idempotent — a second call keeps the first stamp — and clearing forgets it.
    #[tokio::test]
    async fn entering_and_clearing_stamp_and_forget_the_time() {
        let root = temp_root();
        let app = test_app(&root);
        assert!(!app.drain.draining() && app.drain.since().is_none());
        app.drain.enter();
        let stamped = app.drain.since().expect("entering stamps a time");
        assert!(app.drain.draining());
        app.drain.enter();
        assert_eq!(app.drain.since(), Some(stamped), "a second enter keeps the first stamp");
        app.drain.clear();
        assert!(!app.drain.draining() && app.drain.since().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    /// `drain_and_wait` returns as soon as nothing is booting or publishing, and times out rather
    /// than hang otherwise. Paused time fires the poll without waiting it out.
    #[tokio::test(start_paused = true)]
    async fn the_wait_returns_when_the_queue_is_quiet_and_times_out_otherwise() {
        let root = temp_root();
        let app = test_app(&root);
        // Quiet: returns at once, and draining is set by the call itself.
        assert!(drain_and_wait(&app, Duration::from_secs(5)).await);
        assert!(app.drain.draining(), "the wait enters draining");

        // One booting colony holds it: the short timeout fires and it goes on anyway.
        let mut starting = colony("acme", SessionStatus::Starting);
        starting.id = "booting".into();
        app.sessions.write().await.push(starting);
        assert_eq!(in_flight(&app).await, 1);
        assert!(
            !drain_and_wait(&app, Duration::from_secs(3)).await,
            "a boot that never leaves starting runs the wait out"
        );

        // Once it stops holding the status, the wait returns true.
        app.update_session("booting", |x| x.status = SessionStatus::Running).await;
        assert!(drain_and_wait(&app, Duration::from_secs(3)).await);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The routes: POST enters (and cancels) draining and reports it; GET reads the state back,
    /// counting the in-flight colonies.
    #[tokio::test]
    async fn the_drain_routes_enter_cancel_and_report() {
        let root = temp_root();
        let app = test_app(&root);
        let mut publishing = colony("acme", SessionStatus::Publishing);
        publishing.id = "pushing".into();
        app.sessions.write().await.push(publishing);

        let Json(entered) = post(State(app.clone()), Bytes::new()).await.unwrap();
        assert_eq!(entered["draining"], true);
        assert_eq!(entered["in_flight"], 1);
        assert_eq!(entered["ready"], false, "a publish is still in flight");
        assert!(entered["since"].is_string(), "entering stamps the time: {entered}");

        let Json(read) = get(State(app.clone())).await.unwrap();
        assert_eq!(read, entered, "GET reads the state POST set");

        let Json(cancelled) = post(State(app.clone()), Bytes::from_static(br#"{"draining": false}"#))
            .await
            .unwrap();
        assert_eq!(cancelled["draining"], false);
        assert!(cancelled["since"].is_null(), "cancelling forgets the stamp");

        let bad = post(State(app.clone()), Bytes::from_static(b"not json")).await;
        assert_eq!(bad.unwrap_err().status(), StatusCode::BAD_REQUEST);

        let _ = std::fs::remove_dir_all(root);
    }
}
