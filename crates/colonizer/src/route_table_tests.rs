//! The route table, snapshotted: every API route's method and path, what an unauthenticated
//! request gets, what the scoped-token allowlist (`api_tokens::classify`) decides for it, and the
//! activity-log kind it records. Rebuilding how the router is assembled must leave this file
//! unchanged; a pull request that adds or changes a route updates `routes.snap` in the same diff,
//! so the change to the API's surface and its access rules is visible in review.
//!
//! Regenerate with `UPDATE_ROUTE_SNAPSHOT=1 cargo test -p colonizer-harness route_table`.
//!
//! How it reads the table without running a handler: the candidate paths are every `.route("/api…"`
//! literal in the crate's sources, and each is asked with `OPTIONS`, a method no API route
//! registers. The router answers from the matched route's method fallback, a 405 whose `Allow`
//! header lists the methods the route has, and a probe route layer reports which route matched.
use std::{collections::BTreeSet, path::Path};

use axum::{
    body::Body,
    extract::{MatchedPath, Request},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::Response,
};
use tower::ServiceExt as _;

const SNAPSHOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/routes.snap");

/// Every string literal passed to `.route(` under `src/`, that starts with `/api`.
fn candidate_paths() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for (at, _) in text.match_indices(".route(") {
                    let rest = text[at + ".route(".len()..].trim_start();
                    if let Some(literal) = rest.strip_prefix('"')
                        && let Some(end) = literal.find('"')
                        && literal[..end].starts_with("/api")
                    {
                        out.insert(literal[..end].to_string());
                    }
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
    out
}

/// A request path for a route template: each `{param}` becomes `x`.
fn concrete(template: &str) -> String {
    template
        .split('/')
        .map(|seg| if seg.starts_with('{') { "x" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

/// Stamps the matched route on the response, then lets the request go on to the route's method
/// fallback (the only thing an `OPTIONS` request reaches).
async fn probe(req: Request, next: Next) -> Response {
    let matched = req.extensions().get::<MatchedPath>().map(|m| m.as_str().to_string());
    let mut res = next.run(req).await;
    if let Some(m) = matched {
        res.headers_mut().insert("x-route", HeaderValue::from_str(&m).unwrap());
    }
    res
}

fn request(method: Method, uri: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7878")
        .body(Body::empty())
        .unwrap()
}

/// The table as `routes.snap` holds it: one line per method and route, sorted by path.
async fn route_table() -> String {
    let root = std::env::temp_dir().join(format!("colonizer-routes-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    let probed = crate::api_routes()
        .route_layer(middleware::from_fn(probe))
        .with_state(app.clone());
    let full = crate::router(&app);

    let mut lines = Vec::new();
    for template in candidate_paths() {
        let uri = concrete(&template);
        let res = probed.clone().oneshot(request(Method::OPTIONS, &uri)).await.unwrap();
        if res.headers().get("x-route").and_then(|v| v.to_str().ok()) != Some(template.as_str()) {
            // Not a route of the real API: a test router's path, or one another template shadows.
            continue;
        }
        assert_eq!(
            res.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "OPTIONS {template} reached a handler"
        );
        let allow = res
            .headers()
            .get(header::ALLOW)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let mut methods: Vec<&str> = allow
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty() && *m != "HEAD")
            .collect();
        methods.sort();
        for method in methods {
            let method = Method::from_bytes(method.as_bytes()).unwrap();
            // host_guard answers every unauthenticated API request before routing, except the
            // public status (reduced to an allowlist; its own test covers the body).
            let unauth = if method == Method::GET && template == "/api/status" {
                "public".to_string()
            } else {
                let res = full.clone().oneshot(request(method.clone(), &uri)).await.unwrap();
                res.status().as_u16().to_string()
            };
            let token = crate::api_tokens::describe_need(&method, &uri);
            let activity = crate::activity::recorded_kind(&method, &template).unwrap_or("-");
            lines.push(format!(
                "{template:<52} {:<6} unauth={unauth:<6} token={token:<16} activity={activity}",
                method.as_str()
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    lines.sort();
    format!("{}\n", lines.join("\n"))
}

#[tokio::test]
async fn the_route_table_matches_its_snapshot() {
    let table = route_table().await;
    assert!(
        table.lines().count() > 100,
        "the probe found too few routes to be the real table:\n{table}"
    );
    if std::env::var_os("UPDATE_ROUTE_SNAPSHOT").is_some() {
        std::fs::write(SNAPSHOT, &table).unwrap();
        return;
    }
    let saved = std::fs::read_to_string(SNAPSHOT).unwrap_or_default();
    if saved != table {
        let old: BTreeSet<&str> = saved.lines().collect();
        let new: BTreeSet<&str> = table.lines().collect();
        let gone: Vec<_> = old.difference(&new).collect();
        let added: Vec<_> = new.difference(&old).collect();
        panic!(
            "the route table changed; if that is intended, regenerate routes.snap with \
             UPDATE_ROUTE_SNAPSHOT=1 cargo test -p colonizer-harness route_table\n\
             no longer served:\n{gone:#?}\nnewly served:\n{added:#?}"
        );
    }
}
