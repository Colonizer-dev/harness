//! History search: the owner route's filters and caps, the gateway's org/sensitivity scoping, and the
//! snippet and date handling. Kept out of `history.rs` so the module proper stays readable.

use super::*;
use crate::sessions::tests::colony;
use crate::tests::{temp_root, test_app_with};
use axum::{
    Router,
    body::Body,
    http::{Method, Request},
};
use std::collections::HashMap;
use tower::ServiceExt as _;

/// Run one request and return its status and parsed JSON body.
async fn call(router: Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = router.oneshot(req).await.unwrap();
    let status = res.status();
    let body: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
    (status, body)
}

fn get_req(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn post_req(uri: &str, body: &str, token: Option<&str>) -> Request<Body> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        req = req.header(header::AUTHORIZATION, token);
    }
    req.body(Body::from(body.to_owned())).unwrap()
}

/// The `hits` array, and the colonies it names, sorted.
fn hits(body: &Value) -> Vec<Value> {
    body["hits"].as_array().cloned().unwrap_or_default()
}

fn colonies(body: &Value) -> Vec<String> {
    let mut out: Vec<String> = hits(body)
        .iter()
        .filter_map(|h| h["colony"].as_str().map(str::to_string))
        .collect();
    out.sort();
    out
}

/// A colony with the fields history search reads.
fn col(id: &str, org: &str, repo: &str, class: Option<&str>) -> Session {
    let mut s = colony(org, SessionStatus::Idle);
    s.id = id.into();
    s.agent = "claude".into();
    s.sensitivity = class.map(str::to_string);
    if !repo.is_empty() {
        s.repo = repo.into();
    }
    s
}

/// Pushes a colony, stamping `created_at` so the first pushed is the newest — search order is then the
/// order the tests push, which is what the `limit` case asserts.
async fn push(app: &Shared, mut s: Session) -> Session {
    let mut all = app.sessions.write().await;
    s.created_at = Utc::now() - chrono::TimeDelta::try_seconds(all.len() as i64).unwrap();
    all.push(s.clone());
    s
}

/// Write a colony's `events.jsonl` from the given events.
fn write_events(app: &Shared, id: &str, events: &[Value]) {
    std::fs::create_dir_all(app.session_dir(id)).unwrap();
    let text: String = events.iter().map(|e| format!("{e}\n")).collect();
    std::fs::write(app.session_dir(id).join("events.jsonl"), text).unwrap();
}

/// A live gateway token for a colony, written where `colony_for_token` reads it.
fn token_for(app: &Shared, id: &str) -> String {
    let token = crate::util::random_token();
    std::fs::create_dir_all(app.session_dir(id)).unwrap();
    std::fs::write(app.gateway_token_file(id), token.as_bytes()).unwrap();
    token
}

fn user_event(seq: u64, id: &str, text: &str) -> Value {
    json!({"seq": seq, "ts": "2026-01-01T00:00:00Z", "type": "user_message", "id": id, "text": text})
}

fn assistant_event(seq: u64, message_id: &str, text: &str) -> Value {
    json!({"seq": seq, "ts": "2026-01-01T00:00:01Z", "type": "assistant_text", "message_id": message_id, "block_index": 0, "text": text})
}

/// (a) The owner route filters and caps; the query is case-insensitive and every term must appear; an
/// empty query or a bad date is a 400.
#[tokio::test]
async fn the_owner_route_filters_caps_and_refuses_bad_input() {
    let root = temp_root();
    let app = test_app_with(&root, |_| {});
    push(&app, col("a1", "acme", "", None)).await;
    let mut a2 = col("a2", "acme", "", None);
    a2.agent = "codex".into();
    a2.status = SessionStatus::Merged;
    push(&app, a2).await;
    push(&app, col("b1", "other", "", None)).await;
    write_events(&app, "a1", &[user_event(1, "u1", "Deploy the PIPELINE now")]);
    write_events(&app, "a2", &[assistant_event(2, "m1", "the deploy pipeline is green")]);
    write_events(&app, "b1", &[user_event(3, "u2", "deploy pipeline for other org")]);
    let router = routes().with_state(app.clone());
    let owner = |uri: &str| {
        let (router, uri) = (router.clone(), uri.to_string());
        async move { call(router, get_req(&uri)).await }
    };

    // Both terms, any case; the owner route is unscoped, so all three colonies match.
    let (status, body) = owner("/api/history/search?q=deploy+pipeline").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hits(&body).len(), 3, "{body}");
    assert!(hits(&body).iter().any(|h| h["role"] == "assistant" && h["turn"] == "m1"));
    assert!(hits(&body).iter().any(|h| h["role"] == "user" && h["turn"] == "u1"));

    // One term missing, and each filter narrowing the set.
    assert!(hits(&owner("/api/history/search?q=deploy+missing").await.1).is_empty());
    assert_eq!(hits(&owner("/api/history/search?q=deploy&org=acme").await.1).len(), 2);
    for (uri, want) in [
        ("q=deploy&agent=codex", vec!["a2"]),
        ("q=deploy&status=merged", vec!["a2"]),
        ("q=deploy&status=parked", vec![]),
        ("q=deploy&repo=acme/nope", vec![]),
        ("q=deploy&limit=1", vec!["a1"]),
    ] {
        assert_eq!(colonies(&owner(&format!("/api/history/search?{uri}")).await.1), want, "{uri}");
    }

    // An empty query and a bad date are 400; a bare day and a naive datetime both parse.
    for (uri, want) in [
        ("q=", StatusCode::BAD_REQUEST),
        ("q=x&since=nope", StatusCode::BAD_REQUEST),
        ("q=x&until=nope", StatusCode::BAD_REQUEST),
        ("q=deploy&since=2026-01-01", StatusCode::OK),
        ("q=deploy&since=2026-01-01T00:00:00", StatusCode::OK),
    ] {
        assert_eq!(owner(&format!("/api/history/search?{uri}")).await.0, want, "{uri}");
    }
    std::fs::remove_dir_all(&root).ok();
}

/// No colony may contribute more than [`PER_COLONY`] hits.
#[tokio::test]
async fn one_colony_contributes_at_most_three_hits() {
    let root = temp_root();
    let app = test_app_with(&root, |_| {});
    push(&app, col("a1", "acme", "", None)).await;
    let events: Vec<Value> = (1..=6)
        .map(|i| user_event(i, &format!("u{i}"), "the word match here"))
        .collect();
    write_events(&app, "a1", &events);
    assert_eq!(search(&app, "match", DEFAULT_LIMIT, |_| true).await.len(), PER_COLONY);
    std::fs::remove_dir_all(&root).ok();
}

/// (b) The gateway route: only same-org neighbours, never itself or another org, and a protected
/// (restricted, missing or unparseable) log only to a `restricted` caller; a bad token is a 401.
#[tokio::test]
async fn the_gateway_route_scopes_to_org_and_sensitivity() {
    let root = temp_root();
    let app = test_app_with(&root, |_| {});
    // Acme callers (one classed, one restricted) and acme neighbours that are classed, restricted, or
    // unclassified (None, which fails closed); then an org-less caller, its org-less same-repo
    // neighbour, and "trick" — the same repo but in an org, so hidden from an org-less caller.
    for (id, org, repo, class) in [
        ("me", "acme", "", Some("standard")),
        ("metoo", "acme", "", Some("restricted")),
        ("friend", "acme", "", Some("standard")),
        ("secret", "acme", "", Some("restricted")),
        ("plain", "acme", "", None),
        ("outsider", "other", "", Some("standard")),
        ("nobody", "", "solo/repo", Some("standard")),
        ("solomate", "", "solo/repo", Some("standard")),
        ("trick", "acme", "solo/repo", Some("standard")),
    ] {
        push(&app, col(id, org, repo, class)).await;
        write_events(&app, id, &[user_event(1, "u", "the keyword here")]);
    }
    let tokens: HashMap<String, String> = ["me", "metoo", "nobody"]
        .iter()
        .map(|id| (id.to_string(), token_for(&app, id)))
        .collect();
    let router = crate::gateway::router(app.clone());
    let ask = |id: &str| {
        let (router, bearer) = (router.clone(), format!("Bearer {}", tokens[id]));
        async move { call(router, post_req("/history", r#"{"query":"keyword"}"#, Some(&bearer))).await }
    };

    // The non-restricted caller sees the classed acme neighbours, but no protected log.
    let (status, body) = ask("me").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(colonies(&body), ["friend", "trick"], "protected logs stay hidden: {body}");
    // The restricted caller also sees the restricted and unclassified acme neighbours.
    assert_eq!(colonies(&ask("metoo").await.1), ["friend", "me", "plain", "secret", "trick"]);
    // The org-less caller sees only its org-less same-repo neighbour.
    assert_eq!(colonies(&ask("nobody").await.1), ["solomate"], "never read an org's colonies");
    // A wrong token, or none, is a 401 — the body is valid, so the handler speaks, not the extractor.
    for token in [Some("Bearer nope"), None] {
        assert_eq!(
            call(router.clone(), post_req("/history", r#"{"query":"x"}"#, token)).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    std::fs::remove_dir_all(&root).ok();
}

/// (c) Snippets stay on char boundaries in non-ASCII text, and the offset map survives a lowercase form
/// whose length differs ('İ' becomes two chars).
#[test]
fn snippets_and_offsets_are_char_boundary_safe_for_non_ascii() {
    let text = format!("{}需要修复的部署流水线{}", "日本語".repeat(100), "다음".repeat(100));
    let snippet = snippet_around(&text, text.find("部署").unwrap());
    assert!(snippet.contains("部署"), "{snippet}");
    assert!(
        snippet.chars().count() <= SNIPPET_CHARS + 2,
        "plus two ellipses: {}",
        snippet.chars().count()
    );
    assert!(snippet.chars().all(|c| c != '\u{FFFD}'), "{snippet}");
    assert!(snippet_around(&text, text.find("다음").unwrap()).chars().count() <= SNIPPET_CHARS + 2);
    assert_eq!(snippet_around("héllo wörld", 0), "héllo wörld");
    assert_eq!(snippet_around("", 0), "");

    // 'İ' lowercases to two chars, so the lowercased offset differs from the original's.
    let text = "İİİtail";
    let lower = text.to_lowercase();
    let at = map_offset(text, &lower, lower.find("tail").unwrap());
    assert_eq!(at, text.find("tail").unwrap());
    assert!(text.is_char_boundary(at));
}

#[test]
fn since_and_until_accept_rfc3339_a_naive_datetime_or_a_bare_day() {
    assert_eq!(
        parse_when("2026-03-01", false).unwrap().to_rfc3339(),
        "2026-03-01T00:00:00+00:00"
    );
    assert!(parse_when("2026-03-01", true).unwrap() > parse_when("2026-03-01", false).unwrap());
    assert_eq!(
        parse_when("2026-03-01T06:00:00", false).unwrap().to_rfc3339(),
        "2026-03-01T06:00:00+00:00"
    );
    assert!(parse_when("2026-03-01T06:00:00Z", false).is_some());
    assert!(parse_when("not a date", false).is_none());
}
