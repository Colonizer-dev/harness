//! The history drain against a fake owner (axum on 127.0.0.1) that records what arrives and in
//! what order, and can be told to refuse, throttle, or hang; and once against the real owner.

use super::*;
use crate::sessions::{Session, SessionStatus, tests::colony};
use axum::{
    Router,
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A temp root that removes itself.
struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const TOKEN: &str = "col_member_token";
/// The refresh credential the fake owner accepts, and the token a refresh hands back (#762).
const REFRESH: &str = "refresh_credential";
const FRESH: &str = "col_fresh_token";

/// What the fake owner saw, and how it is told to misbehave.
#[derive(Default)]
struct Owner {
    requests: usize,
    /// `payload:<sha>` and `row:<id>`, in arrival order.
    events: Vec<String>,
    payloads: BTreeSet<String>,
    /// Upserted by id, like the real owner.
    rows: BTreeMap<String, Value>,
    /// Every row id received, re-sends included.
    received: Vec<String>,
    batch_sizes: Vec<usize>,
    batch_bytes: Vec<usize>,
    /// Rows that arrived before one of their payloads.
    order_violations: Vec<String>,
    /// Forced answers for the next requests, any route: status and `Retry-After`.
    script: VecDeque<(u16, Option<&'static str>)>,
    forbid: bool,
    /// A batch holding a row with this summary is refused whole, naming no row.
    poison: Option<String>,
    /// The Nth accepted rows batch is stored, then never answered.
    hang_on_batch: Option<usize>,
    batches: usize,
    /// The owner stopped accepting [`TOKEN`]: only [`FRESH`] authenticates (#762).
    rotated: bool,
    /// Token refreshes asked for.
    refreshes: usize,
    /// A forced answer to every refresh: its status.
    refuse_refresh: Option<u16>,
    /// What a refresh hands back instead of [`FRESH`].
    refresh_gives: Option<&'static str>,
}

#[derive(Clone)]
struct Fake {
    owner: Arc<Mutex<Owner>>,
    hung: Arc<tokio::sync::Notify>,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            owner: Arc::new(Mutex::new(Owner::default())),
            hung: Arc::new(tokio::sync::Notify::new()),
        }
    }
    fn with<R>(&self, f: impl FnOnce(&mut Owner) -> R) -> R {
        f(&mut self.owner.lock().unwrap())
    }
}

/// Authentication, the removed flag and the script — what every fake route answers first.
fn gate(o: &mut Owner, headers: &HeaderMap) -> Option<Response> {
    o.requests += 1;
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if bearer != Some(if o.rotated { FRESH } else { TOKEN }) {
        return Some(StatusCode::UNAUTHORIZED.into_response());
    }
    if o.forbid {
        return Some((StatusCode::FORBIDDEN, Json(json!({"error": "not a member"}))).into_response());
    }
    if let Some((status, retry)) = o.script.pop_front() {
        let mut res = (StatusCode::from_u16(status).unwrap(), Json(json!({"error": "scripted"}))).into_response();
        if let Some(retry) = retry {
            res.headers_mut().insert(header::RETRY_AFTER, retry.parse().unwrap());
        }
        return Some(res);
    }
    None
}

async fn fake_payload(State(f): State<Fake>, UrlPath(sha): UrlPath<String>, headers: HeaderMap, body: Bytes) -> Response {
    let mut o = f.owner.lock().unwrap();
    if let Some(res) = gate(&mut o, &headers) {
        return res;
    }
    assert_eq!(sha256_hex(&body), sha, "a payload travels under its own hash");
    o.events.push(format!("payload:{sha}"));
    o.payloads.insert(sha);
    StatusCode::NO_CONTENT.into_response()
}

async fn fake_rows(State(f): State<Fake>, headers: HeaderMap, body: Bytes) -> Response {
    let answered: Option<Response> = {
        let mut o = f.owner.lock().unwrap();
        if let Some(res) = gate(&mut o, &headers) {
            return res;
        }
        let v: Value = serde_json::from_slice(&body).unwrap();
        let rows = v["rows"].as_array().unwrap().clone();
        o.batch_sizes.push(rows.len());
        o.batch_bytes.push(body.len());
        if let Some(poison) = o.poison.clone()
            && rows.iter().any(|r| r["record"]["summary"] == poison.as_str())
        {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": "a row does not parse"})),
            )
                .into_response();
        }
        o.batches += 1;
        let mut accepted = Vec::new();
        for r in &rows {
            let id = r["id"].as_str().unwrap().to_string();
            for p in r["payloads"].as_array().unwrap() {
                if p["omitted"] != true && !o.payloads.contains(p["sha256"].as_str().unwrap()) {
                    o.order_violations.push(id.clone());
                }
            }
            o.events.push(format!("row:{id}"));
            o.received.push(id.clone());
            o.rows.insert(id.clone(), r.clone());
            accepted.push(id);
        }
        (o.hang_on_batch != Some(o.batches)).then(|| Json(json!({"accepted": accepted})).into_response())
    };
    if let Some(res) = answered {
        return res;
    }
    // Stored, never acknowledged: the member is killed while it waits.
    f.hung.notify_one();
    std::future::pending::<Response>().await
}

/// The owner's refresh door: the member id and credential in the body are the whole
/// authentication; a good pair rotates the owner onto [`FRESH`].
async fn fake_refresh(State(f): State<Fake>, Json(body): Json<Value>) -> Response {
    let mut o = f.owner.lock().unwrap();
    o.refreshes += 1;
    if let Some(status) = o.refuse_refresh {
        return (StatusCode::from_u16(status).unwrap(), Json(json!({"error": "refused"}))).into_response();
    }
    if body["member_id"] != "mem_1" || body["refresh"] != REFRESH {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "no match"}))).into_response();
    }
    o.rotated = true;
    Json(json!({"token": o.refresh_gives.unwrap_or(FRESH)})).into_response()
}

async fn serve(fake: &Fake) -> String {
    let router = Router::new()
        .route("/api/fleet/peer/refresh", axum::routing::post(fake_refresh))
        .route("/api/fleet/peer/payloads/{sha256}", axum::routing::put(fake_payload))
        .route("/api/fleet/peer/rows", axum::routing::post(fake_rows))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    url
}

/// A member data dir with `n` merged colonies `s00…`, each with its own `events.jsonl`, plus one
/// still running (never drained). `summary` names each colony's summary.
fn member(n: usize, summary: impl Fn(usize) -> String) -> (TempRoot, PathBuf) {
    let root = TempRoot(std::env::temp_dir().join(format!("colonizer-fleet-sync-{}", util::short_id())));
    let data = root.0.join("data");
    std::fs::create_dir_all(&data).unwrap();
    write_colonies(&data, n, &summary);
    (root, data)
}

fn write_colonies(data: &Path, n: usize, summary: &dyn Fn(usize) -> String) {
    let base = Utc::now() - chrono::Duration::hours(1);
    let mut sessions: Vec<Session> = (0..n)
        .map(|i| {
            let mut s = colony("acme", SessionStatus::Merged);
            s.id = format!("s{i:02}");
            s.summary = Some(summary(i));
            s.created_at = base + chrono::Duration::seconds(i as i64);
            s.updated_at = s.created_at;
            let dir = data.join("sessions").join(&s.id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("events.jsonl"), format!("{{\"colony\":{i}}}\n")).unwrap();
            s
        })
        .collect();
    let mut live = colony("acme", SessionStatus::Running);
    live.id = "live".into();
    sessions.push(live);
    std::fs::write(data.join("sessions.json"), serde_json::to_vec(&sessions).unwrap()).unwrap();
}

fn origin() -> Origin {
    Origin {
        host: "hostA".into(),
        name: "member".into(),
    }
}

fn target(url: &str) -> Target {
    Target {
        owner_url: url.to_string(),
        member_id: "mem_1".into(),
        token: TOKEN.into(),
        refresh: None,
    }
}

/// [`target`], holding the refresh credential approval minted.
fn refreshable(url: &str) -> Target {
    Target {
        refresh: Some(REFRESH.into()),
        ..target(url)
    }
}

fn cfg(max_rows: usize) -> DrainConfig {
    DrainConfig {
        max_rows,
        ..DrainConfig::default()
    }
}

fn plain(i: usize) -> String {
    format!("colony {i}")
}

/// A clock that never sleeps: it records each wait and moves its time forward by it.
struct FakeClock {
    now: Mutex<DateTime<Utc>>,
    slept: Mutex<Vec<Duration>>,
}

impl FakeClock {
    fn new() -> FakeClock {
        FakeClock {
            now: Mutex::new(Utc::now()),
            slept: Mutex::new(Vec::new()),
        }
    }
    fn advance(&self, by: Duration) {
        *self.now.lock().unwrap() += chrono::Duration::from_std(by).unwrap();
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap()
    }
    fn sleep(&self, wait: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.slept.lock().unwrap().push(wait);
        self.advance(wait);
        Box::pin(async {})
    }
}

#[tokio::test]
async fn payloads_are_acknowledged_before_the_rows_that_reference_them() {
    let fake = Fake::new();
    let url = serve(&fake).await;
    let (_root, data) = member(5, plain);

    let report = drain(&data, &origin(), &target(&url), &cfg(2), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Synced, "{report:?}");
    assert_eq!((report.sent, report.payloads, report.pending), (5, 5, 0));

    fake.with(|o| {
        assert!(
            o.order_violations.is_empty(),
            "rows before payloads: {:?}",
            o.order_violations
        );
        assert_eq!(o.rows.len(), 5, "the running colony is not drained");
        assert!(o.rows.contains_key("hostA:s00"));
        // Each row's payload arrived earlier in the stream than the row itself.
        for (id, row) in &o.rows {
            let sha = row["payloads"][0]["sha256"].as_str().unwrap();
            let payload_at = o.events.iter().position(|e| *e == format!("payload:{sha}")).unwrap();
            let row_at = o.events.iter().position(|e| *e == format!("row:{id}")).unwrap();
            assert!(payload_at < row_at, "{id}");
        }
        assert_eq!(o.batch_sizes, vec![2, 2, 1]);
    });

    // A second drain has nothing to send, and sends nothing.
    let before = fake.with(|o| o.requests);
    let again = drain(&data, &origin(), &target(&url), &cfg(2), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!((again.sent, again.pending), (0, 0));
    assert_eq!(fake.with(|o| o.requests), before);
}

#[tokio::test]
async fn a_changed_row_is_sent_again_and_upserted() {
    let fake = Fake::new();
    let url = serve(&fake).await;
    let (_root, data) = member(3, plain);
    drain(&data, &origin(), &target(&url), &cfg(10), &SystemClock, false)
        .await
        .unwrap();

    // One record changes (its logs do not): only that row's fingerprint moves.
    let mut sessions = fleet_export::read_sessions(&data).unwrap();
    sessions[1].summary = Some("merged later".into());
    std::fs::write(data.join("sessions.json"), serde_json::to_vec(&sessions).unwrap()).unwrap();
    let report = drain(&data, &origin(), &target(&url), &cfg(10), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.sent, 1, "only the changed row travels");
    fake.with(|o| {
        assert_eq!(o.rows.len(), 3);
        assert_eq!(o.rows["hostA:s01"]["record"]["summary"], "merged later");
    });
}

#[tokio::test]
async fn a_401_stops_the_drain_and_asks_for_attention() {
    let fake = Fake::new();
    let url = serve(&fake).await;
    let (_root, data) = member(4, plain);
    let mut stale = target(&url);
    stale.token = "col_revoked".into();

    let report = drain(&data, &origin(), &stale, &cfg(10), &SystemClock, false).await.unwrap();
    assert_eq!(report.status, SyncStatus::Unauthorized);
    assert!(report.detail.unwrap().contains("re-join"));
    assert_eq!(fake.with(|o| o.requests), 1, "the first 401 ends the drain");
    let state = DrainState::load(&data);
    assert_eq!(state.status, SyncStatus::Unauthorized);
    // What member health reads (#764): the backlog it left, and since when.
    assert_eq!(state.backlog_rows, 4);
    assert!(state.backlog_since.is_some());

    // The background drain stays stopped; a manual one tries again.
    let quiet = drain(&data, &origin(), &stale, &cfg(10), &SystemClock, false).await.unwrap();
    assert!(quiet.skipped);
    assert_eq!(fake.with(|o| o.requests), 1);
    let forced = drain(&data, &origin(), &target(&url), &cfg(10), &SystemClock, true)
        .await
        .unwrap();
    assert_eq!(forced.status, SyncStatus::Synced);
    assert_eq!(fake.with(|o| o.rows.len()), 4);
    let state = DrainState::load(&data);
    assert_eq!(
        (state.backlog_rows, state.backlog_since),
        (0, None),
        "a drained queue has no backlog"
    );
}

/// Issue #762: a 401 trades the refresh credential for a fresh token once, the drain retries on it
/// and goes on to the end, and the report hands the new token back to be stored.
#[tokio::test]
async fn a_401_refreshes_the_token_and_the_drain_succeeds() {
    let fake = Fake::new();
    fake.with(|o| o.rotated = true);
    let url = serve(&fake).await;
    let (_root, data) = member(4, plain);

    let report = drain(&data, &origin(), &refreshable(&url), &cfg(2), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Synced, "{report:?}");
    assert_eq!(report.sent, 4);
    assert_eq!(report.refreshed_token.as_deref(), Some(FRESH));
    assert_eq!(
        fake.with(|o| o.refreshes),
        1,
        "one refresh, then every request carries the new token"
    );
    assert_eq!(fake.with(|o| o.rows.len()), 4);
    assert!(
        !serde_json::to_string(&report).unwrap().contains(FRESH),
        "the token never travels in a report"
    );
}

/// Issue #762: a refresh the owner refuses stops the drain as unauthorized; so does a second 401
/// on the refreshed token — one refresh per drain, never a loop.
#[tokio::test]
async fn a_401_whose_refresh_fails_stops_as_unauthorized() {
    let fake = Fake::new();
    fake.with(|o| {
        o.rotated = true;
        o.refuse_refresh = Some(401);
    });
    let url = serve(&fake).await;
    let (_root, data) = member(3, plain);
    let report = drain(&data, &origin(), &refreshable(&url), &cfg(10), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Unauthorized);
    assert!(report.detail.unwrap().contains("re-join"));
    assert_eq!(report.refreshed_token, None);
    assert_eq!(fake.with(|o| (o.refreshes, o.requests)), (1, 1));
    assert_eq!(DrainState::load(&data).status, SyncStatus::Unauthorized);

    // The refresh answers, but with a token the owner refuses as well: the second 401 stops.
    let fake = Fake::new();
    fake.with(|o| {
        o.rotated = true;
        o.refresh_gives = Some("col_still_refused");
    });
    let url = serve(&fake).await;
    let (_root, data) = member(3, plain);
    let report = drain(&data, &origin(), &refreshable(&url), &cfg(10), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Unauthorized);
    assert_eq!(fake.with(|o| (o.refreshes, o.requests)), (1, 2), "one refresh per drain");
    assert_eq!(fake.with(|o| o.rows.len()), 0);
}

/// Issue #762: a 403 is a removal, and a removed member never refreshes.
#[tokio::test]
async fn a_403_never_refreshes() {
    let fake = Fake::new();
    fake.with(|o| o.forbid = true);
    let url = serve(&fake).await;
    let (_root, data) = member(2, plain);
    let report = drain(&data, &origin(), &refreshable(&url), &cfg(10), &SystemClock, true)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Removed);
    assert_eq!(fake.with(|o| o.refreshes), 0);

    // A refresh the owner answers 403 (its tombstone for a removed member) is a removal too.
    let fake = Fake::new();
    fake.with(|o| {
        o.rotated = true;
        o.refuse_refresh = Some(403);
    });
    let url = serve(&fake).await;
    let (_root, data) = member(2, plain);
    let report = drain(&data, &origin(), &refreshable(&url), &cfg(10), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Removed);
}

/// Issue #762 against the real owner: the owner revokes a member's fleet token; the member's next
/// drain refreshes it through `POST /api/fleet/peer/refresh`, the old token is gone for good, and
/// the new one is stored in the membership. Once the owner removes the member, its refresh is a
/// 403 — a tombstoned member is never refreshable — and a wrong credential is a 401.
#[tokio::test]
async fn the_real_owner_rotates_a_revoked_token_and_never_refreshes_a_removed_member() {
    let owner_root = TempRoot(std::env::temp_dir().join(format!("colonizer-fleet-owner-{}", util::short_id())));
    std::fs::create_dir_all(owner_root.0.join("config")).unwrap();
    let owner = crate::tests::test_app(&owner_root.0);
    let (member_id, token, refresh) = crate::fleet_members::FleetStore::add_refreshable_member_for_tests(&owner, "worker").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let served = owner.clone();
    tokio::spawn(async move { axum::serve(listener, crate::server::router(&served)).await.unwrap() });

    // The owner revokes the member's token in its token list; the member is still a member.
    let old_id = owner.fleet_members.token_id_for_tests(&member_id).await.unwrap();
    owner.api_tokens.revoke(&old_id).await.unwrap();

    // The member's app, consented, drains through the background path that stores the new token.
    let member_root = TempRoot(std::env::temp_dir().join(format!("colonizer-fleet-member-{}", util::short_id())));
    std::fs::create_dir_all(member_root.0.join("config")).unwrap();
    let app = crate::tests::test_app(&member_root.0);
    write_colonies(&app.cfg.data_dir, 3, &plain);
    let to = Target {
        owner_url: url.clone(),
        member_id: member_id.clone(),
        token: token.clone(),
        refresh: Some(refresh.clone()),
    };
    app.fleet_members.set_membership_for_tests(Some(to.clone())).await;
    app.fleet_members.set_history_sync(true).await;
    let report = trigger(State(app.clone())).await.unwrap().0;
    assert_eq!((report.status, report.sent), (SyncStatus::Synced, 3), "{report:?}");
    let stored = app.fleet_members.membership().await.unwrap();
    assert_ne!(stored.token, token, "the membership holds the rotated token");
    assert_ne!(
        owner.fleet_members.token_id_for_tests(&member_id).await.unwrap(),
        old_id,
        "the owner rotated the member's token"
    );
    let refresh_url = format!("{url}/api/fleet/peer/refresh");
    let ask = |member: &str, secret: &str| {
        reqwest::Client::new()
            .post(&refresh_url)
            .json(&json!({"member_id": member, "refresh": secret}))
            .send()
    };
    assert_eq!(
        ask(&member_id, "wrong").await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a wrong credential refreshes nothing"
    );

    // Removed: the tombstone answers the old token and the refresh alike with 403.
    crate::fleet_members::FleetStore::remove_member_for_tests(&owner, &member_id).await;
    assert_eq!(
        ask(&member_id, &refresh).await.unwrap().status(),
        reqwest::StatusCode::FORBIDDEN,
        "a removed member is never refreshable"
    );
    write_colonies(&app.cfg.data_dir, 4, &plain);
    let report = drain(&app.cfg.data_dir, &origin(), &stored, &cfg(2), &SystemClock, true)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Removed, "{report:?}");
    assert_eq!(report.refreshed_token, None);
}

/// Issue #764: what a member reports to its owner — state, backlog, error class and consent —
/// from the persisted drain state alone.
#[test]
fn the_health_summary_carries_state_backlog_error_class_and_consent() {
    let now = Utc::now();
    let t = target("http://owner:7878");
    let mut state = DrainState {
        member_id: Some("mem_1".into()),
        status: SyncStatus::Error,
        backlog_rows: 7,
        backlog_since: Some(now - chrono::Duration::minutes(90)),
        ..DrainState::default()
    };
    let summary = summary_of(&state, &t, true, now);
    assert_eq!(
        summary,
        json!({"state": "error", "backlog_rows": 7, "oldest_unsent_age_s": 5400,
               "last_error_class": "error", "consent": true})
    );

    for (status, class) in [
        (SyncStatus::Unauthorized, "unauthorized"),
        (SyncStatus::Removed, "forbidden"),
        (SyncStatus::Backoff, "rate_limited"),
    ] {
        state.status = status;
        assert_eq!(summary_of(&state, &t, true, now)["last_error_class"], class, "{status:?}");
    }
    state.status = SyncStatus::Synced;
    assert_eq!(summary_of(&state, &t, true, now)["last_error_class"], Value::Null);

    // Consent off: no backlog is claimed, the state says why.
    let off = summary_of(&state, &t, false, now);
    assert_eq!(off["state"], "consent_required");
    assert_eq!(
        (off["backlog_rows"].clone(), off["oldest_unsent_age_s"].clone()),
        (json!(0), Value::Null)
    );
    assert_eq!(off["consent"], false);

    // A previous membership's state does not count for this one.
    state.member_id = Some("mem_old".into());
    let fresh = summary_of(&state, &t, true, now);
    assert_eq!(fresh["state"], "idle");
    assert_eq!(fresh["backlog_rows"], 0);
}

#[tokio::test]
async fn a_403_stops_syncing_and_keeps_everything_local() {
    let fake = Fake::new();
    fake.with(|o| o.forbid = true);
    let url = serve(&fake).await;
    let (_root, data) = member(3, plain);

    let report = drain(&data, &origin(), &target(&url), &cfg(10), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Removed);
    assert_eq!(report.sent, 0);
    let state = DrainState::load(&data);
    assert_eq!(state.status, SyncStatus::Removed, "recorded for member health");
    assert!(state.acked.is_empty());
    // Nothing local went anywhere.
    assert_eq!(fleet_export::read_sessions(&data).unwrap().len(), 4);
    assert!(data.join("sessions/s00/events.jsonl").exists());

    let quiet = drain(&data, &origin(), &target(&url), &cfg(10), &SystemClock, false)
        .await
        .unwrap();
    assert!(quiet.skipped, "a removed member stops syncing");
    assert_eq!(fake.with(|o| o.requests), 1);
}

#[tokio::test]
async fn a_429_waits_out_its_retry_after() {
    let fake = Fake::new();
    fake.with(|o| o.script.push_back((429, Some("7"))));
    let url = serve(&fake).await;
    let (_root, data) = member(2, plain);
    let clock = FakeClock::new();

    let report = drain(&data, &origin(), &target(&url), &cfg(10), &clock, false).await.unwrap();
    assert_eq!(report.status, SyncStatus::Synced);
    assert_eq!(*clock.slept.lock().unwrap(), vec![Duration::from_secs(7)]);
    assert_eq!(fake.with(|o| o.rows.len()), 2);
}

#[tokio::test]
async fn a_long_503_backs_off_until_its_retry_after() {
    let fake = Fake::new();
    fake.with(|o| o.script.push_back((503, Some("600"))));
    let url = serve(&fake).await;
    let (_root, data) = member(2, plain);
    let clock = FakeClock::new();
    let started = clock.now();

    let report = drain(&data, &origin(), &target(&url), &cfg(10), &clock, false).await.unwrap();
    assert_eq!(report.status, SyncStatus::Backoff);
    assert_eq!(report.next_attempt_at, Some(started + chrono::Duration::seconds(600)));
    assert!(
        clock.slept.lock().unwrap().is_empty(),
        "past max_inline_wait: no inline sleep"
    );

    // Before the time: nothing is sent.
    clock.advance(Duration::from_secs(599));
    let early = drain(&data, &origin(), &target(&url), &cfg(10), &clock, false).await.unwrap();
    assert!(early.skipped);
    assert_eq!(fake.with(|o| o.requests), 1);

    clock.advance(Duration::from_secs(2));
    let later = drain(&data, &origin(), &target(&url), &cfg(10), &clock, false).await.unwrap();
    assert_eq!(later.status, SyncStatus::Synced);
    assert_eq!(fake.with(|o| o.rows.len()), 2);
}

#[tokio::test]
async fn a_poison_row_is_bisected_out_of_a_batch_of_eight_and_retired() {
    let fake = Fake::new();
    fake.with(|o| o.poison = Some("poison".into()));
    let url = serve(&fake).await;
    let (_root, data) = member(8, |i| if i == 5 { "poison".into() } else { plain(i) });
    let config = DrainConfig {
        max_rows: 8,
        max_attempts: 2,
        ..DrainConfig::default()
    };

    let first = drain(&data, &origin(), &target(&url), &config, &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(first.sent, 7, "the seven good rows get through");
    assert_eq!(first.failed, vec!["hostA:s05".to_string()]);
    assert!(first.retired.is_empty(), "one attempt of two");
    assert_eq!(first.pending, 1);
    // 8 refused → 4 good + 4 refused → 2 refused + 2 good → the poison row alone.
    fake.with(|o| {
        assert_eq!(o.batch_sizes, vec![8, 4, 4, 2, 1, 1, 2]);
        assert_eq!(o.rows.len(), 7);
    });

    // Its second attempt travels alone and retires it; the queue is empty behind it.
    let second = drain(&data, &origin(), &target(&url), &config, &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(second.retired, vec!["hostA:s05".to_string()]);
    assert_eq!((second.sent, second.pending), (0, 0));
    assert_eq!(second.status, SyncStatus::Synced);
    fake.with(|o| assert_eq!(o.batch_sizes.last(), Some(&1)));
    let state = DrainState::load(&data);
    assert!(state.retired["hostA:s05"].error.contains("does not parse"));

    // A retired row stays out of every later drain.
    let before = fake.with(|o| o.requests);
    drain(&data, &origin(), &target(&url), &config, &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(fake.with(|o| o.requests), before);
}

#[tokio::test]
async fn a_drain_killed_mid_batch_resumes_without_duplicates() {
    let fake = Fake::new();
    fake.with(|o| o.hang_on_batch = Some(2));
    let url = serve(&fake).await;
    let (_root, data) = member(10, plain);

    let task = {
        let (data, target) = (data.clone(), target(&url));
        tokio::spawn(async move { drain(&data, &origin(), &target, &cfg(3), &SystemClock, false).await })
    };
    tokio::time::timeout(Duration::from_secs(20), fake.hung.notified())
        .await
        .unwrap();
    task.abort();
    let _ = task.await;
    // The owner stored batch two, but never said so: only batch one counts as sent.
    assert_eq!(DrainState::load(&data).acked.len(), 3);
    assert_eq!(fake.with(|o| o.rows.len()), 6);

    fake.with(|o| o.hang_on_batch = None);
    let report = drain(&data, &origin(), &target(&url), &cfg(3), &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.status, SyncStatus::Synced);
    assert_eq!(report.sent, 7);
    fake.with(|o| {
        assert_eq!(o.rows.len(), 10, "every row once on the owner");
        assert_eq!(o.received.len(), 13, "batch two was re-sent");
        let unique: BTreeSet<&String> = o.received.iter().collect();
        assert_eq!(unique.len(), 10);
    });
    assert_eq!(DrainState::load(&data).acked.len(), 10);
}

#[tokio::test]
async fn the_byte_cap_splits_batches() {
    let fake = Fake::new();
    let url = serve(&fake).await;
    let (_root, data) = member(6, |i| format!("{i}{}", "x".repeat(300)));
    let sizes: Vec<usize> = collect(&data, &origin(), &DrainState::default())
        .unwrap()
        .iter()
        .map(|p| p.body_bytes)
        .collect();
    let cap = BODY_OVERHEAD + sizes.iter().max().unwrap() * 2 + 1;
    let config = DrainConfig {
        max_rows: 100,
        max_bytes: cap,
        ..DrainConfig::default()
    };

    let report = drain(&data, &origin(), &target(&url), &config, &SystemClock, false)
        .await
        .unwrap();
    assert_eq!(report.sent, 6);
    fake.with(|o| {
        assert_eq!(o.batch_sizes, vec![2, 2, 2], "the row cap alone would send one batch");
        assert!(o.batch_bytes.iter().all(|b| *b <= cap), "{:?} over {cap}", o.batch_bytes);
    });
}

#[test]
fn batches_respect_both_caps_and_let_an_oversized_row_travel_alone() {
    assert_eq!(
        batches(&[400, 400, 400, 1500, 10], 10, 1000),
        vec![vec![0, 1], vec![2], vec![3], vec![4]]
    );
    assert_eq!(batches(&[1, 1, 1, 1, 1], 2, 1000), vec![vec![0, 1], vec![2, 3], vec![4]]);
    assert!(batches(&[], 2, 1000).is_empty());
}

#[test]
fn retry_after_reads_seconds_and_dates_and_is_capped() {
    let now = Utc::now();
    let mut headers = reqwest::header::HeaderMap::new();
    assert_eq!(retry_after(&headers, now), DEFAULT_RETRY_AFTER);
    headers.insert(reqwest::header::RETRY_AFTER, "12".parse().unwrap());
    assert_eq!(retry_after(&headers, now), Duration::from_secs(12));
    headers.insert(reqwest::header::RETRY_AFTER, "999999".parse().unwrap());
    assert_eq!(retry_after(&headers, now), MAX_RETRY_AFTER);
    let at = (now + chrono::Duration::seconds(90)).to_rfc2822();
    headers.insert(reqwest::header::RETRY_AFTER, at.parse().unwrap());
    let wait = retry_after(&headers, now).as_secs();
    assert!((89..=90).contains(&wait), "{wait}");
}

/// The member against the real owner router: the ingest routes on a fleet token, the rows
/// upserted under the member's directory, a row without its payload refused by name, and the
/// removed member's revoked token read as 401.
#[tokio::test]
async fn a_member_drains_into_a_real_owner() {
    let owner_root = TempRoot(std::env::temp_dir().join(format!("colonizer-fleet-owner-{}", util::short_id())));
    std::fs::create_dir_all(owner_root.0.join("config")).unwrap();
    let owner = crate::tests::test_app(&owner_root.0);
    let (member_id, token) = crate::fleet_members::FleetStore::add_member_for_tests(&owner, "worker").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let served = owner.clone();
    tokio::spawn(async move { axum::serve(listener, crate::server::router(&served)).await.unwrap() });

    let (_root, data) = member(3, plain);
    let to = Target {
        owner_url: url.clone(),
        member_id: member_id.clone(),
        token: token.clone(),
        refresh: None,
    };
    let report = drain(&data, &origin(), &to, &cfg(2), &SystemClock, false).await.unwrap();
    assert_eq!(report.status, SyncStatus::Synced, "{report:?}");
    assert_eq!(report.sent, 3);

    let ingest = owner.cfg.data_dir.join(INGEST_DIR).join(&member_id);
    let stored: BTreeMap<String, Value> = serde_json::from_slice(&std::fs::read(ingest.join(ROWS_FILE)).unwrap()).unwrap();
    assert_eq!(stored.len(), 3);
    let sha = stored["hostA:s00"]["payloads"][0]["sha256"].as_str().unwrap().to_string();
    assert_eq!(
        std::fs::read(ingest.join("payloads").join(&sha)).unwrap(),
        b"{\"colony\":0}\n"
    );

    // A row whose payload never arrived is refused by name, with the hash it lacks.
    let mut row =
        json!({"id": "hostA:s00", "record": stored["hostA:s00"]["record"], "payloads": stored["hostA:s00"]["payloads"]});
    let missing = "a".repeat(64);
    row["payloads"][0]["sha256"] = json!(missing);
    let answer: Value = reqwest::Client::new()
        .post(format!("{url}/api/fleet/peer/rows"))
        .bearer_auth(&token)
        .json(&json!({"rows": [row]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answer["accepted"], json!([]));
    assert_eq!(answer["rejected"][0]["missing_payloads"], json!([missing]));

    // The owner's own cockpit token is no member: 403.
    let res = reqwest::Client::new()
        .post(format!("{url}/api/fleet/peer/rows"))
        .bearer_auth(&owner.api_token)
        .json(&json!({"rows": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::FORBIDDEN);

    // Removed: the owner keeps the revoked token's tombstone, so the next drain reads 403 and
    // stops as `removed` — with every local file where it was.
    crate::fleet_members::FleetStore::remove_member_for_tests(&owner, &member_id).await;
    write_colonies(&data, 4, &plain);
    let report = drain(&data, &origin(), &to, &cfg(2), &SystemClock, true).await.unwrap();
    assert_eq!(report.status, SyncStatus::Removed, "{report:?}");
    assert_eq!(DrainState::load(&data).status, SyncStatus::Removed);
    assert_eq!(fleet_export::read_sessions(&data).unwrap().len(), 5);
    assert!(data.join("sessions/s03/events.jsonl").exists());
    let quiet = drain(&data, &origin(), &to, &cfg(2), &SystemClock, false).await.unwrap();
    assert!(quiet.skipped, "a removed member stops syncing");

    // A token the owner never minted is still the anonymous 401.
    let unknown = Target {
        token: "col_never_minted".into(),
        ..to.clone()
    };
    let report = drain(&data, &origin(), &unknown, &cfg(2), &SystemClock, true).await.unwrap();
    assert_eq!(report.status, SyncStatus::Unauthorized);
}

/// Joining is not consent: the manual trigger refuses and the status says why until the operator
/// has seen the preview and said yes, and a re-join starts it off again.
#[tokio::test]
async fn history_sync_waits_for_consent_and_a_rejoin_resets_it() {
    let fake = Fake::new();
    let url = serve(&fake).await;
    let root = TempRoot(std::env::temp_dir().join(format!("colonizer-fleet-consent-{}", util::short_id())));
    std::fs::create_dir_all(root.0.join("config")).unwrap();
    let app = crate::tests::test_app(&root.0);
    write_colonies(&app.cfg.data_dir, 3, &plain);
    app.fleet_members.set_membership_for_tests(Some(target(&url))).await;

    assert_eq!(app.fleet_members.history_sync().await, Some(false), "off at join");
    let refused = trigger(State(app.clone())).await.expect_err("no consent, no drain");
    assert_eq!(refused.0, StatusCode::CONFLICT);
    let status = view(State(app.clone())).await.0;
    assert_eq!(status["status"], "consent_required");
    assert_eq!(status["consent"], false);
    assert!(status["detail"].as_str().unwrap().contains("preview"));
    assert_eq!(fake.with(|o| o.requests), 0, "nothing left the machine");

    // The preview counts exactly what would go: three finished colonies and their logs.
    let shown = preview(State(app.clone())).await.unwrap().0;
    let log_bytes: u64 = (0..3).map(|i| format!("{{\"colony\":{i}}}\n").len() as u64).sum();
    assert_eq!(shown["colonies"], 3);
    assert_eq!(shown["payloads"], 3);
    assert_eq!(shown["payload_bytes"], log_bytes);
    assert_eq!(shown["pending_colonies"], 3);
    assert_eq!(shown["owner_url"], url);
    assert_eq!(fake.with(|o| o.requests), 0, "a preview sends nothing");

    let answer = consent(State(app.clone()), Json(ConsentBody { enabled: true }))
        .await
        .unwrap()
        .0;
    assert_eq!(answer["consent"], true);
    let report = trigger(State(app.clone())).await.unwrap().0;
    assert_eq!((report.status, report.sent), (SyncStatus::Synced, 3));
    assert_eq!(preview(State(app.clone())).await.unwrap().0["pending_colonies"], 0);

    // Withdrawn: refused again.
    let _ = consent(State(app.clone()), Json(ConsentBody { enabled: false }))
        .await
        .unwrap();
    assert_eq!(trigger(State(app.clone())).await.unwrap_err().0, StatusCode::CONFLICT);

    // Leave and re-join: a new membership starts with consent off.
    let _ = consent(State(app.clone()), Json(ConsentBody { enabled: true }))
        .await
        .unwrap();
    app.fleet_members.set_membership_for_tests(None).await;
    let refused = trigger(State(app.clone())).await.unwrap_err();
    assert_eq!(refused.0, StatusCode::CONFLICT, "not a member");
    let mut again = target(&url);
    again.member_id = "mem_2".into();
    app.fleet_members.set_membership_for_tests(Some(again)).await;
    assert_eq!(app.fleet_members.history_sync().await, Some(false));
    assert_eq!(view(State(app.clone())).await.0["status"], "consent_required");
    assert_eq!(fake.with(|o| o.rows.len()), 3, "nothing more was sent");
}
