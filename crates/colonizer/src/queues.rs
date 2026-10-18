//! The cockpit's Queues page in one read (issue #1127): `GET /api/queues` answers with every
//! colony that is queue material — queued, blocked, parked, waiting on an answer, or carrying the
//! watchdog's attention flag — in queue order, plus the fleet's host rows and the two switches
//! that colour the whole page (the update drain and the external-writes kill switch).
//!
//! It is a read and nothing else: no state, no background work, no activity. The per-row actions
//! (`resume`, `stop`, `restart`) are predictions of what the real endpoints would do with the
//! colony right now, so the page can grey a button out instead of answering a 409 after the click;
//! each refused action says why in `why_not`, reusing the endpoint's own messages.

use crate::{
    ApiResult, Shared,
    api_tokens::Need,
    sessions::{Session, SessionStatus},
};
use axum::{
    Json,
    extract::{Query, State},
    http::Method,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Why the cockpit cannot press an action on a row: named only for the actions that are refused,
/// so a missing key means the button is live.
#[derive(Serialize, Default)]
struct WhyNot {
    #[serde(skip_serializing_if = "Option::is_none")]
    resume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restart: Option<String>,
}

/// The per-row action predictions: what `POST /api/sessions/{id}/resume|stop` and
/// `POST /api/update/restart` would each do with this colony right now.
#[derive(Serialize, Default)]
struct Actions {
    resume: bool,
    stop: bool,
    restart: bool,
}

/// One colony the Queues page shows.
#[derive(Serialize)]
struct QueueRow {
    id: String,
    org: String,
    repo: String,
    issue: Option<u64>,
    title: Option<String>,
    branch: Option<String>,
    host: Option<String>,
    agent: Option<String>,
    status: String,
    created_at: String,
    priority: i64,
    reason: Option<String>,
    detail: Option<String>,
    attention: Option<Value>,
    resumes_at: Option<String>,
    held: bool,
    policy_hold: bool,
    actions: Actions,
    why_not: WhyNot,
}

/// One fleet host row: the numbers [`crate::fleet`] reports, plus the local host's share of the
/// colony counts. `running`/`parked`/`queued` are `null` for a peer: this mothership cannot see
/// another member's colony list, only its status numbers.
#[derive(Serialize)]
struct QueueHost {
    name: String,
    reachable: bool,
    slots_in_use: usize,
    slots_ceiling: usize,
    running: Option<usize>,
    parked: Option<usize>,
    queued: Option<usize>,
    queue_depth: usize,
    over_ceiling: bool,
}

/// A row's place in the line: what [`crate::queue_priority::queue_key`] returns — higher priority
/// first, the older colony within a priority.
type QueueKey = (std::cmp::Reverse<i64>, DateTime<Utc>);

/// The whole page: the queue rows in queue order, the fleet, and the install-wide switches.
#[derive(Serialize)]
struct Queues {
    rows: Vec<QueueRow>,
    hosts: Vec<QueueHost>,
    draining: bool,
    external_writes_blocked: bool,
    queue_depth: usize,
}

/// The optional filters, combined with AND; empty and absent mean the same thing.
#[derive(Deserialize, Default)]
struct QueueQuery {
    host: Option<String>,
    reason: Option<String>,
    repo: Option<String>,
    agent: Option<String>,
    q: Option<String>,
}

/// This module's feature descriptor (`features.rs`). Read-only: reads are never recorded as
/// activity, and a scoped token with the plain `read` scope may watch the queue.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "queues",
    routes,
    token_scope: Some(token_scope),
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

/// What a scoped token needs for these routes: watching the queue is a read, like the colony list.
fn token_scope<'a>(method: &Method, segs: &[&'a str]) -> Option<Need<'a>> {
    match segs {
        ["api", "queues"] if *method == Method::GET => Some(Need::Bare(crate::api_tokens::Scope::Read)),
        _ => None,
    }
}

/// The API routes this module serves.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/queues", routing::get(queues))
}

/// `GET /api/queues`: everything the Queues page draws, computed from the state the other modules
/// already keep.
async fn queues(State(app): State<Shared>, Query(query): Query<QueueQuery>) -> ApiResult<Queues> {
    let sessions = app.sessions.read().await.clone();
    let orgs = app.all_org_settings();
    let now = Utc::now();
    let behind: BTreeSet<String> = crate::update_notices::behind(&sessions, app.cfg.assets.as_deref())
        .into_iter()
        .map(|s| s.id.clone())
        .collect();
    let restarting = crate::update_notices::restarting(&app).await;

    // The fleet (this host first, as `fleet::list_hosts` orders it), then every colony attributed
    // to the host its placement pinned it to, or the local host — cross-member launch is not built
    // (issue #1252), so every colony that exists runs here and only `hosts[0]` ever carries
    // colony counts; of those, the queue-material ones become rows.
    let mut hosts: Vec<QueueHost> = crate::fleet::list_hosts(&app)
        .await
        .into_iter()
        .map(|summary| QueueHost {
            name: summary.name,
            reachable: summary.health == crate::fleet::HostHealth::Online,
            slots_in_use: summary.slots_in_use,
            slots_ceiling: summary.slots_ceiling,
            running: None,
            parked: None,
            queued: None,
            queue_depth: summary.queue_depth,
            over_ceiling: summary.slots_in_use > summary.slots_ceiling,
        })
        .collect();
    let mut rows: Vec<(QueueKey, QueueRow)> = Vec::new();
    for s in &sessions {
        // Every colony counts against its host — a host's running colonies are exactly the ones
        // the queue is not holding — while only queue material becomes a row.
        let host = placed_at(s.placement.as_deref(), &hosts);
        if host == Some(0) {
            count(&mut hosts[0], s);
        }
        if !queue_material(s) {
            continue;
        }
        if !filter_matches(&query, s, host.and_then(|i| hosts.get(i)).map(|h| h.name.as_str())) {
            continue;
        }
        let key = crate::queue_priority::queue_key(crate::queue_priority::effective(s, &orgs, now), s.created_at);
        rows.push((key, row(s, &orgs, now, &behind, &restarting, host.and_then(|i| hosts.get(i)))));
    }
    rows.sort_by_key(|(key, _)| *key);
    let queue_depth = sessions.iter().filter(|s| s.status == SessionStatus::Queued).count();
    Ok(Json(Queues {
        rows: rows.into_iter().map(|(_, row)| row).collect(),
        hosts,
        draining: app.drain.draining(),
        external_writes_blocked: crate::authority::external_writes_blocked(),
        queue_depth,
    }))
}

/// Adds a local colony to its host's counts: `running` is what holds a microVM slot — the same
/// predicate `slots_in_use` counts — beside the parked and the plainly queued.
fn count(host: &mut QueueHost, s: &Session) {
    if s.holds_slot() {
        host.running = Some(host.running.unwrap_or(0) + 1);
    }
    if s.status == SessionStatus::Parked {
        host.parked = Some(host.parked.unwrap_or(0) + 1);
    }
    if s.status == SessionStatus::Queued {
        host.queued = Some(host.queued.unwrap_or(0) + 1);
    }
}

/// Whether a colony is queue material: waiting on the queue itself (`queued`), on another colony
/// (`blocked`), set aside (`parked`), waiting on its person (`waiting_for_answer`), or flagged for
/// attention by the watchdog — whatever else its status says. Everything else (running, idle,
/// publishing, terminal) is not waiting for anything.
fn queue_material(s: &Session) -> bool {
    matches!(
        s.status,
        SessionStatus::Queued | SessionStatus::Blocked | SessionStatus::Parked | SessionStatus::WaitingForAnswer
    ) || s.attention.is_some()
}

/// Which fleet host a colony belongs to: the one its placement pinned it to when the record names
/// one, else the local host — index 0, which `fleet::list_hosts` always puts first. `None` only
/// with no hosts at all, which cannot happen (a fleet is never empty of its own host).
fn placed_at(placement: Option<&str>, hosts: &[QueueHost]) -> Option<usize> {
    let pinned = placement
        .and_then(|p| p.strip_prefix("pinned to "))
        .map(str::trim)
        .filter(|name| !name.is_empty());
    hosts
        .iter()
        .position(|host| Some(host.name.as_str()) == pinned)
        .or(if hosts.is_empty() { None } else { Some(0) })
}

/// The machine reason a row is in the queue, using the same keys the cockpit's cards already map
/// to wording: a park's own reason, the waiting-on-a-person state, the blocked state, the
/// watchdog's attention reason, or none of those for a plainly queued colony.
fn reason(s: &Session) -> Option<String> {
    let attention = || s.attention.as_ref().and_then(|a| a["reason"].as_str()).map(str::to_string);
    match s.status {
        SessionStatus::Parked => s.parked.as_ref().map(|p| p.reason.clone()).or_else(attention),
        SessionStatus::WaitingForAnswer => Some("waiting_for_answer".to_string()),
        SessionStatus::Blocked => Some("blocked".to_string()),
        _ => attention(),
    }
}

/// The human context text under the reason: why a colony is blocked, or the error its last run
/// left, when either is on the record.
fn detail(s: &Session) -> Option<String> {
    if s.status == SessionStatus::Blocked {
        s.blocked_reason.clone()
    } else {
        s.error.clone()
    }
}

/// The colony's title, as `supersede::colony_title` spells it: the issue's title, else the
/// summariser's one-sentence summary, else nothing.
fn title(s: &Session) -> Option<String> {
    let title = s.issue_title.trim();
    if title.is_empty() {
        s.summary.clone().filter(|summary| !summary.trim().is_empty())
    } else {
        Some(title.to_string())
    }
}

/// One row's action predictions and their reasons, from the same predicates the real endpoints
/// check: `resume` from the parked-only gate plus the supersession hold (`supersede::blocks_start`,
/// the thing the resume route refuses on), `stop` from the stop route's own accepted states, and
/// `restart` from the behind set `POST /api/update/restart` works from.
fn actions(s: &Session, behind: &BTreeSet<String>, restarting: &BTreeSet<String>) -> (Actions, WhyNot) {
    let held = crate::supersede::blocks_start(s);
    let policy_hold = crate::playbook::is_security_hold(s.attention.as_ref());
    let parked = s.status == SessionStatus::Parked;

    // A policy hold is the one exception to the parked-only rule: its release is a person's call,
    // which is exactly what the cockpit's resume is. It may not be machine-restarted, though —
    // that is what the hold is for.
    let resume = parked && (!held || policy_hold);
    let why_resume = (!resume).then(|| {
        if held {
            s.superseded
                .as_ref()
                .map(crate::supersede::blocked_message)
                .unwrap_or_else(|| "only parked colonies can be resumed".to_string())
        } else {
            "only parked colonies can be resumed".to_string()
        }
    });

    let stop = !s.status.is_terminal() && s.status != SessionStatus::Publishing;
    let why_stop = (!stop).then(|| {
        if s.status == SessionStatus::Publishing {
            "it is publishing right now"
        } else {
            "already stopped"
        }
        .to_string()
    });

    let in_flight = restarting.contains(s.id.as_str());
    let restart = behind.contains(s.id.as_str()) && !in_flight && !policy_hold;
    let why_restart = (!restart).then(|| {
        if policy_hold {
            "on a release policy hold; release it from the colony's page first"
        } else if in_flight {
            "already restarting"
        } else {
            "already on the current version"
        }
        .to_string()
    });

    (
        Actions { resume, stop, restart },
        WhyNot {
            resume: why_resume,
            stop: why_stop,
            restart: why_restart,
        },
    )
}

/// One queue row out of a colony record.
fn row(
    s: &Session,
    orgs: &BTreeMap<String, crate::orgs::OrgSettings>,
    now: DateTime<Utc>,
    behind: &BTreeSet<String>,
    restarting: &BTreeSet<String>,
    host: Option<&QueueHost>,
) -> QueueRow {
    let (actions, why_not) = actions(s, behind, restarting);
    QueueRow {
        id: s.id.clone(),
        org: s.org.clone(),
        repo: s.repo.clone(),
        issue: s.issue,
        title: title(s),
        branch: (!s.branch.is_empty()).then(|| s.branch.clone()),
        host: host.map(|host| host.name.clone()),
        agent: (!s.agent.is_empty()).then(|| s.agent.clone()),
        status: s.status.as_str().to_string(),
        created_at: s.created_at.to_rfc3339(),
        priority: crate::queue_priority::effective(s, orgs, now),
        reason: reason(s),
        detail: detail(s),
        attention: s.attention.clone(),
        resumes_at: s.parked.as_ref().and_then(|p| p.resets_at.clone()),
        held: crate::supersede::blocks_start(s),
        policy_hold: crate::playbook::is_security_hold(s.attention.as_ref()),
        actions,
        why_not,
    }
}

/// Whether a colony passes the request's filters, combined with AND; an absent or empty filter is
/// no filter. `host` is judged on the attributed host (the pinned member, else the local one),
/// `repo` accepts the bare name as well as `org/repo`, and `q` is a case-insensitive substring
/// over the repo, issue number, title, branch and id.
fn filter_matches(query: &QueueQuery, s: &Session, host: Option<&str>) -> bool {
    /// A filter that names nothing (`""`, blank) filters nothing.
    fn clean(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }
    if let Some(want) = clean(query.host.as_deref())
        && host != Some(want)
    {
        return false;
    }
    if let Some(want) = clean(query.reason.as_deref())
        && reason(s).as_deref() != Some(want)
    {
        return false;
    }
    if let Some(want) = clean(query.repo.as_deref()) {
        let named = s.repo == want || s.repo.rsplit_once('/').is_some_and(|(_, name)| name == want);
        if !named {
            return false;
        }
    }
    if let Some(want) = clean(query.agent.as_deref())
        && s.agent != want
    {
        return false;
    }
    if let Some(q) = clean(query.q.as_deref()) {
        let hay = format!(
            "{} {} {} {} {}",
            s.repo,
            s.issue.map(|i| i.to_string()).unwrap_or_default(),
            s.issue_title,
            s.branch,
            s.id
        )
        .to_lowercase();
        if !hay.contains(&q.to_lowercase()) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{Park, tests::colony};
    use crate::supersede::{OverlapReason, Supersession};
    use crate::tests::{temp_root, test_app, test_app_with};
    use axum::{body::Body, http::header};
    use chrono::Duration;
    use serde_json::json;
    use tower::ServiceExt as _;

    /// A colony with an id and an age, everything else default.
    fn one(id: &str, org: &str, status: SessionStatus, age_minutes: i64) -> Session {
        let mut s = colony(org, status);
        s.id = id.into();
        s.created_at = Utc::now() - Duration::minutes(age_minutes);
        s
    }

    /// The watchdog's attention flag, as `watchdog` writes it.
    fn attention(reason: &str) -> Value {
        json!({"reason": reason, "since": Utc::now().to_rfc3339(), "nudges": 1})
    }

    /// An unkept supersession hold, as the PR watcher writes one at a merge edge.
    fn superseded(kept: bool) -> Supersession {
        Supersession {
            by: "merger".into(),
            pr_url: "https://github.com/acme/repo/pull/9".into(),
            pr: Some(9),
            title: "the covering pull request".into(),
            reason: OverlapReason::Files,
            at: Utc::now(),
            kept,
        }
    }

    fn parked(id: &str, reason: &str, resets_at: Option<&str>) -> Session {
        let mut s = one(id, "acme", SessionStatus::Parked, 10);
        s.parked = Some(Park {
            at: Utc::now(),
            reason: reason.into(),
            resets_at: resets_at.map(str::to_string),
            vm_kept: false,
            question_risk: None,
        });
        s
    }

    /// `GET /api/queues` through the module's own router, with the loopback Host the guard wants.
    async fn get(app: &Shared, query: &str) -> Value {
        let uri = if query.is_empty() {
            "/api/queues".to_string()
        } else {
            format!("/api/queues?{query}")
        };
        let req = axum::http::Request::builder()
            .method(axum::http::Method::GET)
            .uri(&uri)
            .header(header::HOST, "127.0.0.1:7878")
            .body(Body::empty())
            .unwrap();
        let res = routes().with_state(app.clone()).oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK, "{uri}");
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn app_with(sessions: Vec<Session>) -> (Shared, std::path::PathBuf) {
        let root = temp_root();
        let app = test_app(&root);
        *app.sessions.write().await = sessions;
        (app, root)
    }

    /// The response's rows keyed by id, and their ids in the order the answer carries them.
    fn rows(queues: &Value) -> (Vec<String>, BTreeMap<String, Value>) {
        let array = queues["rows"].as_array().unwrap();
        let ids = array.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
        let by_id = array
            .iter()
            .map(|r| (r["id"].as_str().unwrap().to_string(), r.clone()))
            .collect();
        (ids, by_id)
    }

    #[tokio::test]
    async fn the_queue_holds_the_waiting_states_in_queue_order_and_nothing_else() {
        let mut high = one("queued-high", "acme", SessionStatus::Queued, 10);
        high.priority = Some(5);
        let mut held_back = one("attentioned", "acme", SessionStatus::Running, 1);
        held_back.attention = Some(attention("stalled"));
        let mut set_aside = parked("parked", "provider_quota_exhausted", None);
        set_aside.created_at = Utc::now() - Duration::minutes(20);
        let (app, root) = app_with(vec![
            one("queued-old", "acme", SessionStatus::Queued, 30),
            high,
            set_aside,
            one("waiting", "acme", SessionStatus::WaitingForAnswer, 15),
            {
                let mut b = one("blocked", "acme", SessionStatus::Blocked, 5);
                b.blocked_reason = Some("waiting on #5 (stopped)".into());
                b
            },
            held_back,
            one("running", "acme", SessionStatus::Running, 2),
            one("stopped", "acme", SessionStatus::Stopped, 3),
            one("merged", "acme", SessionStatus::Merged, 4),
        ])
        .await;

        let queues = get(&app, "").await;
        let (ids, _) = rows(&queues);
        assert_eq!(
            ids,
            vec!["queued-high", "queued-old", "parked", "waiting", "blocked", "attentioned"],
            "higher priority first, then the older colony; running and terminal stay out"
        );
        assert_eq!(queues["queue_depth"], 2, "only the queued colonies count");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_row_names_its_reason_its_detail_and_its_holds() {
        let mut held = parked("held", "provider_quota_exhausted", None);
        held.superseded = Some(superseded(false));
        let mut policy = one("policy", "acme", SessionStatus::Queued, 1);
        policy.attention = Some(attention("control_defeat"));
        let (app, root) = app_with(vec![
            parked("quota-parked", "repo_pr_rate_limit", Some("2026-10-10T00:00:00Z")),
            {
                let mut stalled = one("stalled", "acme", SessionStatus::Queued, 2);
                stalled.attention = Some(attention("stalled"));
                stalled
            },
            {
                let mut b = one("blocked", "acme", SessionStatus::Blocked, 3);
                b.blocked_reason = Some("waiting on #5 (stopped)".into());
                b
            },
            {
                let mut w = one("waiting", "acme", SessionStatus::WaitingForAnswer, 4);
                w.error = Some("the runner died".into());
                w
            },
            one("plain", "acme", SessionStatus::Queued, 5),
            held,
            policy,
        ])
        .await;

        let queues = get(&app, "").await;
        let (_, by_id) = rows(&queues);
        let quota = &by_id["quota-parked"];
        assert_eq!(quota["reason"], "repo_pr_rate_limit", "a park's own reason names the row");
        assert_eq!(
            quota["resumes_at"], "2026-10-10T00:00:00Z",
            "the park's reset time comes through"
        );
        assert_eq!(quota["status"], "parked");
        let stalled = &by_id["stalled"];
        assert_eq!(stalled["reason"], "stalled", "an attention flag names a queued row");
        assert_eq!(
            stalled["attention"]["nudges"], 1,
            "the attention object passes through verbatim"
        );
        let blocked = &by_id["blocked"];
        assert_eq!(blocked["reason"], "blocked");
        assert_eq!(
            blocked["detail"], "waiting on #5 (stopped)",
            "a blocked row's detail is its blocked_reason"
        );
        let waiting = &by_id["waiting"];
        assert_eq!(waiting["reason"], "waiting_for_answer");
        assert_eq!(waiting["detail"], "the runner died", "any other row's detail is its error");
        let plain = &by_id["plain"];
        assert!(plain["reason"].is_null(), "a plainly queued colony has no reason yet");
        assert!(plain["detail"].is_null());
        let held = &by_id["held"];
        assert_eq!(held["held"], true, "an unkept supersession holds the colony");
        assert_eq!(held["policy_hold"], false);
        let policy = &by_id["policy"];
        assert_eq!(policy["policy_hold"], true, "control_defeat is a security hold");
        assert_eq!(policy["held"], false);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_filters_narrow_the_rows_and_combine() {
        let mut on_box = parked("on-box", "provider_quota_exhausted", None);
        on_box.placement = Some("pinned to localhost".into());
        let mut other = one("other", "acme", SessionStatus::Queued, 1);
        other.agent = "codex".into();
        other.repo = "acme/other".into();
        other.issue = Some(12);
        other.issue_title = "the other task".into();
        let (app, root) = app_with(vec![on_box, other]).await;

        let (ids, _) = rows(&get(&app, "").await);
        assert_eq!(ids.len(), 2, "no filter, every row");
        let (ids, _) = rows(&get(&app, "reason=provider_quota_exhausted").await);
        assert_eq!(ids, vec!["on-box"]);
        let (ids, _) = rows(&get(&app, "reason=blocked").await);
        assert!(ids.is_empty(), "no match, empty rows");
        let (ids, _) = rows(&get(&app, "repo=other").await);
        assert_eq!(ids, vec!["other"], "the bare repo name matches");
        let (ids, _) = rows(&get(&app, "repo=acme/other").await);
        assert_eq!(ids, vec!["other"], "so does org/repo");
        let (ids, _) = rows(&get(&app, "agent=codex").await);
        assert_eq!(ids, vec!["other"]);
        let (ids, _) = rows(&get(&app, "q=12").await);
        assert_eq!(ids, vec!["other"], "q reaches the issue number");
        let (ids, _) = rows(&get(&app, "q=THE+OTHER").await);
        assert_eq!(ids, vec!["other"], "q is case-insensitive over the title");
        let host = get(&app, "").await["hosts"][0]["name"].as_str().unwrap().to_string();
        let (ids, _) = rows(&get(&app, &format!("host={host}")).await);
        assert_eq!(ids.len(), 2, "every colony here names the local host");
        assert!(get(&app, "host=nowhere").await["rows"].as_array().unwrap().is_empty());
        let (ids, _) = rows(&get(&app, "agent=codex&q=other").await);
        assert_eq!(ids, vec!["other"], "filters combine with AND");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_host_rows_carry_the_fleet_numbers_and_the_local_counts() {
        let (app, root) = app_with(vec![
            one("first", "acme", SessionStatus::Running, 1),
            one("second", "acme", SessionStatus::Running, 2),
            parked("aside", "provider_quota_exhausted", None),
            one("lined-up", "acme", SessionStatus::Queued, 3),
            one("gone", "acme", SessionStatus::Stopped, 4),
        ])
        .await;

        let queues = get(&app, "").await;
        assert_eq!(
            queues["hosts"].as_array().unwrap().len(),
            1,
            "no peers configured: the local host alone"
        );
        let host = &queues["hosts"][0];
        assert_eq!(host["reachable"], true);
        assert_eq!(host["running"], 2, "the live colonies hold the local host's slots");
        assert_eq!(host["parked"], 1);
        assert_eq!(host["queued"], 1);
        assert_eq!(host["queue_depth"], 1);
        assert_eq!(
            host["over_ceiling"], false,
            "two live colonies under the sandbox default of 3"
        );

        app.drain.enter();
        assert_eq!(get(&app, "").await["draining"], true, "the update drain reads through");
        app.drain.clear();
        assert_eq!(get(&app, "").await["draining"], false);

        let blocked = crate::authority::test_block_external_writes();
        assert_eq!(
            get(&app, "").await["external_writes_blocked"],
            true,
            "the kill switch reads through"
        );
        drop(blocked);
        assert_eq!(get(&app, "").await["external_writes_blocked"], false);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn over_ceiling_is_true_when_more_is_in_use_than_the_ceiling_allows() {
        let (app, root) = app_with(vec![
            one("a", "acme", SessionStatus::Running, 1),
            one("b", "acme", SessionStatus::Running, 2),
            one("c", "acme", SessionStatus::Running, 3),
            one("d", "acme", SessionStatus::Starting, 4),
        ])
        .await;
        // Four live colonies against the sandbox schema's default capacity of three.
        let queues = get(&app, "").await;
        assert_eq!(queues["hosts"][0]["slots_in_use"], 4);
        assert_eq!(queues["hosts"][0]["over_ceiling"], true);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_actions_say_what_each_endpoint_would_do() {
        let mut held = parked("held", "provider_quota_exhausted", None);
        held.superseded = Some(superseded(false));
        let mut kept = parked("kept", "provider_quota_exhausted", None);
        kept.superseded = Some(superseded(true));
        let mut publishing = one("publishing", "acme", SessionStatus::Publishing, 1);
        publishing.attention = Some(attention("stalled"));
        let mut finished = one("finished", "acme", SessionStatus::Stopped, 2);
        finished.attention = Some(attention("stalled"));
        let mut policy = parked("policy", "hold_timeout", None);
        policy.attention = Some(attention("control_defeat"));
        let (app, root) = app_with(vec![
            parked("clean", "idle_timeout", None),
            held,
            kept,
            publishing,
            finished,
            policy,
        ])
        .await;

        let (_, by_id) = rows(&get(&app, "").await);
        let clean = &by_id["clean"];
        assert_eq!(clean["actions"]["resume"], true, "a clean parked colony resumes");
        assert_eq!(clean["actions"]["stop"], true);
        assert_eq!(
            clean["actions"]["restart"], false,
            "the test install has no version to be behind"
        );
        assert_eq!(clean["why_not"]["restart"], "already on the current version");
        assert!(clean["why_not"]["resume"].is_null() && clean["why_not"]["stop"].is_null());
        let held = &by_id["held"];
        assert_eq!(held["actions"]["resume"], false, "a superseded colony waits for its Keep");
        assert!(
            held["why_not"]["resume"]
                .as_str()
                .unwrap()
                .starts_with("superseded by https://github.com/acme/repo/pull/9"),
            "{}",
            held["why_not"]["resume"]
        );
        assert_eq!(
            by_id["kept"]["actions"]["resume"], true,
            "a kept supersession no longer holds"
        );
        let publishing = &by_id["publishing"];
        assert_eq!(publishing["actions"]["stop"], false);
        assert_eq!(publishing["why_not"]["stop"], "it is publishing right now");
        let finished = &by_id["finished"];
        assert_eq!(finished["actions"]["stop"], false, "terminal, however it got attention");
        assert_eq!(finished["why_not"]["stop"], "already stopped");
        let policy = &by_id["policy"];
        assert_eq!(policy["actions"]["resume"], true, "the cockpit is the person-release path");
        assert_eq!(policy["actions"]["restart"], false);
        assert_eq!(
            policy["why_not"]["restart"],
            "on a release policy hold; release it from the colony's page first"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_colony_on_an_older_app_slot_may_be_restarted_and_a_current_one_may_not() {
        let assets = temp_root().join("app");
        std::fs::create_dir_all(&assets).unwrap();
        let root = temp_root();
        let app = test_app_with(&root, |cfg| cfg.assets = Some(assets.clone()));
        // The behind set is the colonies with a microVM (`update_notices::behind`), so both
        // fixtures park with their VM kept: exactly what a queue row that may be restarted is.
        let mut on_old = parked("behind", "idle_timeout", None);
        on_old.parked.as_mut().unwrap().vm_kept = true;
        on_old.app_slot = Some(temp_root().join("other").to_string_lossy().into());
        let mut on_current = parked("current", "idle_timeout", None);
        on_current.parked.as_mut().unwrap().vm_kept = true;
        on_current.app_slot = Some(assets.to_string_lossy().into());
        *app.sessions.write().await = vec![on_old, on_current];

        let (_, by_id) = rows(&get(&app, "").await);
        assert_eq!(by_id["behind"]["actions"]["restart"], true, "not on this build's slot");
        assert!(by_id["behind"]["why_not"]["restart"].is_null());
        assert_eq!(by_id["current"]["actions"]["restart"], false);
        assert_eq!(by_id["current"]["why_not"]["restart"], "already on the current version");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(assets);
    }
}
