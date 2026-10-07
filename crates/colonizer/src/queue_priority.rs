//! Queue priority (issue #1156): who starts next when more colonies are queued than there are
//! slots. The queue orders by `(effective priority desc, created_at asc)`, so with every priority at
//! its default 0 it is the first-in-first-out line it always was.
//!
//! A queued colony's effective priority is its own `priority` when it has one, else its org's
//! `queue_priority`, else 0. The optional starvation guard (the org's `max_wait_hours`) lifts a
//! colony that has waited that long to [`HIGH`], so a low priority colony cannot wait for ever. The
//! global, org and per-repository limits are untouched: priority only decides who is tried first,
//! and a colony whose repository is at its cap is still skipped for the next one that fits
//! (`queue::next_queued`).
//!
//! `POST /api/sessions/{id}/priority {"priority": n}` sets a colony's own priority (`null` clears it
//! back to its org's), `move-to-front` sets it just above everything else queued and `move-to-back`
//! just below. All three are owner-only to scoped API tokens, as every route without a token scope
//! is, and each records an activity line against the colony.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{
    ApiResult, Shared,
    app::client_error,
    orgs::OrgSettings,
    sessions::{Session, SessionStatus},
};

/// The priority the cockpit calls High; also what the starvation guard lifts a colony to.
pub(crate) const HIGH: i64 = 10;

/// The largest priority, up or down, a setting or a request may carry.
pub(crate) const PRIORITY_LIMIT: i32 = 1_000_000;

pub(crate) fn valid_priority(n: i64) -> bool {
    n.unsigned_abs() <= u64::from(PRIORITY_LIMIT.unsigned_abs())
}

/// This module's feature descriptor (`features.rs`). No `token_scope`: the routes stay owner-only.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "queue_priority",
    routes,
    token_scope: None,
    activity: ACTIVITY,
    kinds: &["colony.move_to_back", "colony.move_to_front", "colony.priority"],
    start_tasks: None,
};

const ACTIVITY: &[crate::activity::Rule] = &[
    crate::activity::rule(
        "POST",
        "/api/sessions/{id}/move-to-back",
        "colony.move_to_back",
        crate::activity::Target::Colony,
    ),
    crate::activity::rule(
        "POST",
        "/api/sessions/{id}/move-to-front",
        "colony.move_to_front",
        crate::activity::Target::Colony,
    ),
    crate::activity::rule(
        "POST",
        "/api/sessions/{id}/priority",
        "colony.priority",
        crate::activity::Target::Colony,
    ),
];

/// The API routes this module serves.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/sessions/{id}/move-to-back", routing::post(move_to_back))
        .route("/api/sessions/{id}/move-to-front", routing::post(move_to_front))
        .route("/api/sessions/{id}/priority", routing::post(set_priority))
}

/// A queued colony's place in the line right now: its own priority, else its org's, else 0 — lifted
/// to [`HIGH`] once it has waited its org's `max_wait_hours`.
pub(crate) fn effective(s: &Session, orgs: &BTreeMap<String, OrgSettings>, now: DateTime<Utc>) -> i64 {
    let org = orgs.get(&s.org);
    let base = s.priority.or_else(|| org.and_then(|o| o.queue_priority)).map_or(0, i64::from);
    let starved = org
        .and_then(|o| o.max_wait_hours)
        .filter(|hours| *hours > 0)
        .is_some_and(|hours| now - s.created_at >= chrono::Duration::hours(i64::try_from(hours).unwrap_or(i64::MAX / 3_600_000)));
    if starved { base.max(HIGH) } else { base }
}

/// The ordering key of a queued colony: higher priority first, then the older.
pub(crate) fn queue_key(priority: i64, created_at: DateTime<Utc>) -> (std::cmp::Reverse<i64>, DateTime<Utc>) {
    (std::cmp::Reverse(priority), created_at)
}

/// The extremes of the other queued colonies' effective priorities, for move-to-front/back.
fn others(sessions: &[Session], id: &str, orgs: &BTreeMap<String, OrgSettings>, now: DateTime<Utc>) -> Vec<i64> {
    sessions
        .iter()
        .filter(|s| s.status == SessionStatus::Queued && s.id != id)
        .map(|s| effective(s, orgs, now))
        .collect()
}

/// The priority that puts a colony ahead of everything else queued: one above the highest, and
/// never below `HIGH + 1` so a colony the starvation guard lifted does not outrank it.
pub(crate) fn front_priority(others: &[i64]) -> i64 {
    (others.iter().copied().max().unwrap_or(0).max(HIGH) + 1).min(i64::from(PRIORITY_LIMIT))
}

/// The priority that puts a colony behind everything else queued: one below the lowest, and never
/// above the Low preset's neighbour so the move is a real demotion for an idle queue.
pub(crate) fn back_priority(others: &[i64]) -> i64 {
    (others.iter().copied().min().unwrap_or(0).min(0) - 1).max(-i64::from(PRIORITY_LIMIT))
}

/// Stores `priority` on a queued colony, re-checking its status under the write lock.
async fn apply(app: &Shared, id: &str, priority: Option<i64>, what: &str) -> ApiResult<Session> {
    let value = priority.map(|p| i32::try_from(p).unwrap_or(0));
    let Some((session, changed)) = app
        .update_session(id, |s| {
            if s.status != SessionStatus::Queued {
                return false;
            }
            s.priority = value;
            true
        })
        .await
    else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    if !changed {
        return Err(client_error(
            StatusCode::CONFLICT,
            "only a queued colony has a place in the queue; this one is not queued",
        ));
    }
    app.session_log(id, "info", what.to_string()).await;
    Ok(Json(session))
}

async fn queued(app: &Shared, id: &str) -> Result<Session, crate::app::AppError> {
    let s = app
        .session(id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    if s.status != SessionStatus::Queued {
        return Err(client_error(
            StatusCode::CONFLICT,
            "only a queued colony has a place in the queue; this one is not queued",
        ));
    }
    Ok(s)
}

/// `POST /api/sessions/{id}/priority` with `{"priority": n}`; `null` clears the colony's own
/// priority so it follows its org's again.
pub(crate) async fn set_priority(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Session> {
    let priority = match body.get("priority") {
        Some(Value::Null) => None,
        Some(v) => Some(v.as_i64().filter(|n| valid_priority(*n)).ok_or_else(|| {
            client_error(
                StatusCode::BAD_REQUEST,
                &format!("priority is a whole number from -{PRIORITY_LIMIT} to {PRIORITY_LIMIT}"),
            )
        })?),
        None => {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                "send {\"priority\": n}, or null to follow the org",
            ));
        }
    };
    queued(&app, &id).await?;
    let what = match priority {
        Some(n) => format!("queue priority set to {n}"),
        None => "queue priority cleared; it follows its org again".to_string(),
    };
    apply(&app, &id, priority, &what).await
}

/// `POST /api/sessions/{id}/move-to-front`: just above everything else queued.
pub(crate) async fn move_to_front(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    queued(&app, &id).await?;
    let orgs = app.all_org_settings();
    let sessions = app.sessions.read().await.clone();
    let priority = front_priority(&others(&sessions, &id, &orgs, Utc::now()));
    apply(
        &app,
        &id,
        Some(priority),
        &format!("moved to the front of the queue (priority {priority})"),
    )
    .await
}

/// `POST /api/sessions/{id}/move-to-back`: just below everything else queued.
pub(crate) async fn move_to_back(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Session> {
    queued(&app, &id).await?;
    let orgs = app.all_org_settings();
    let sessions = app.sessions.read().await.clone();
    let priority = back_priority(&others(&sessions, &id, &orgs, Utc::now()));
    apply(
        &app,
        &id,
        Some(priority),
        &format!("moved to the back of the queue (priority {priority})"),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::colony;
    use chrono::Duration;
    use serde_json::json;

    fn queued(id: &str, org: &str, age_minutes: i64) -> Session {
        let mut s = colony(org, SessionStatus::Queued);
        s.id = id.into();
        s.created_at = Utc::now() - Duration::minutes(age_minutes);
        s
    }

    fn orgs(entries: &[(&str, Option<i32>, Option<u64>)]) -> BTreeMap<String, OrgSettings> {
        entries
            .iter()
            .map(|(org, priority, hours)| {
                (
                    (*org).to_string(),
                    OrgSettings {
                        queue_priority: *priority,
                        max_wait_hours: *hours,
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    #[test]
    fn a_colony_follows_its_org_until_it_has_a_priority_of_its_own() {
        let orgs = orgs(&[("hot", Some(10), None), ("cold", Some(-10), None)]);
        let now = Utc::now();
        assert_eq!(effective(&queued("a", "plain", 1), &orgs, now), 0, "no setting is Normal");
        assert_eq!(effective(&queued("b", "hot", 1), &orgs, now), 10);
        assert_eq!(effective(&queued("c", "cold", 1), &orgs, now), -10);
        let mut own = queued("d", "cold", 1);
        own.priority = Some(3);
        assert_eq!(effective(&own, &orgs, now), 3, "its own priority wins over the org's");
    }

    #[test]
    fn the_starvation_guard_lifts_a_colony_that_waited_too_long_and_only_up_to_high() {
        let orgs = orgs(&[("cold", Some(-10), Some(24)), ("off", Some(-10), Some(0))]);
        let now = Utc::now();
        assert_eq!(
            effective(&queued("fresh", "cold", 60), &orgs, now),
            -10,
            "an hour is not enough"
        );
        assert_eq!(effective(&queued("starved", "cold", 25 * 60), &orgs, now), HIGH);
        assert_eq!(
            effective(&queued("zero", "off", 25 * 60), &orgs, now),
            -10,
            "0 hours turns the guard off"
        );
        let mut front = queued("front", "cold", 25 * 60);
        front.priority = Some(50);
        assert_eq!(effective(&front, &orgs, now), 50, "the guard never lowers a priority");
    }

    #[test]
    fn front_and_back_sit_beyond_everything_else_queued() {
        assert_eq!(front_priority(&[]), HIGH + 1);
        assert_eq!(front_priority(&[0, 3]), HIGH + 1, "above High even when nothing is high");
        assert_eq!(front_priority(&[10, 42]), 43);
        assert_eq!(back_priority(&[]), -1);
        assert_eq!(back_priority(&[5, 10]), -1, "behind Normal even when everything is high");
        assert_eq!(back_priority(&[-10, 0]), -11);
        assert_eq!(front_priority(&[i64::from(PRIORITY_LIMIT)]), i64::from(PRIORITY_LIMIT));
    }

    async fn app_with(sessions: Vec<Session>) -> (Shared, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-prio-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        *app.sessions.write().await = sessions;
        (app, root)
    }

    #[tokio::test]
    async fn move_to_front_puts_a_colony_above_the_current_maximum_and_back_below_the_minimum() {
        let mut high = queued("high", "acme", 30);
        high.priority = Some(40);
        let mut low = queued("low", "acme", 20);
        low.priority = Some(-30);
        let (app, root) = app_with(vec![high, low, queued("plain", "acme", 10)]).await;

        let Json(moved) = move_to_front(State(app.clone()), Path("plain".into())).await.unwrap();
        assert_eq!(moved.priority, Some(41));
        let Json(moved) = move_to_back(State(app.clone()), Path("plain".into())).await.unwrap();
        assert_eq!(moved.priority, Some(-31));
        let Json(moved) = set_priority(State(app.clone()), Path("plain".into()), Json(json!({"priority": 7})))
            .await
            .unwrap();
        assert_eq!(moved.priority, Some(7));
        let Json(cleared) = set_priority(State(app.clone()), Path("plain".into()), Json(json!({"priority": null})))
            .await
            .unwrap();
        assert_eq!(cleared.priority, None, "null follows the org again");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn only_a_queued_colony_can_be_moved_and_a_bad_priority_is_refused() {
        let mut running = queued("run", "acme", 5);
        running.status = SessionStatus::Running;
        let (app, root) = app_with(vec![running, queued("q", "acme", 1)]).await;

        let refused = move_to_front(State(app.clone()), Path("run".into())).await.unwrap_err();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
        let missing = move_to_front(State(app.clone()), Path("nope".into())).await.unwrap_err();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        for body in [
            json!({}),
            json!({"priority": "high"}),
            json!({"priority": 1.5}),
            json!({"priority": 2_000_000}),
        ] {
            let bad = set_priority(State(app.clone()), Path("q".into()), Json(body))
                .await
                .unwrap_err();
            assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        }
        assert_eq!(app.session("q").await.unwrap().priority, None, "nothing was stored");
        let _ = std::fs::remove_dir_all(root);
    }
}
