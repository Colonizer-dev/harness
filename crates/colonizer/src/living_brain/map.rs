//! The brain map for the cockpit's 3D view (`GET /api/brain/graph`).
//!
//! The mothership reads the brain's `GET /v1/graph` with the saved token and answers the cockpit
//! with structure only: an index per node, its kind, its degree, its label (or, in structure-only
//! mode, a keyed hash of it) and the links between indices. Nothing else of the brain crosses —
//! no slug, URL, scope, dates, facts, backlinks, page bodies, search. The token is read here and
//! never leaves this process; the brain's answer is cached for [`CACHE_TTL`], the route is
//! rate-limited, the read is size-capped, and a request that came through the remote tunnel always
//! gets structure only. Which colony sits on which node is decided here too: the cockpit names the
//! repositories it has, and the answer says which node each one matches.

use super::{Client, load, token, update};
use crate::{AppError, Shared, client_error, remote};
use axum::{
    Extension, Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
};
use chrono::Utc;
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

/// How long one read of the brain's map is reused before the brain is asked again.
pub(crate) const CACHE_TTL: Duration = Duration::from_secs(60);
/// The most nodes kept (the brain's own ceiling for `limit` is 2000).
pub(crate) const MAX_NODES: usize = 2000;
/// The most links kept.
const MAX_LINKS: usize = 20_000;
/// The largest answer read from the brain, in bytes; a larger one is refused, not truncated.
const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Requests this route answers a minute, cached or not; beyond that, 429.
const RATE_PER_MINUTE: usize = 60;
/// The most repositories one request may ask the matches for.
const MAX_REPOS: usize = 100;
const MAX_LABEL: usize = 160;
const MAX_KIND: usize = 40;
const MAX_DEGREE: u64 = 100_000;

/// One node, as the cockpit may see it: its label, kind and degree. Nothing else.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Node {
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) degree: u64,
}

/// The brain's map reduced to structure: nodes in the brain's order, and links between their indices.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Shaped {
    pub(crate) nodes: Vec<Node>,
    pub(crate) links: Vec<(usize, usize)>,
    pub(crate) truncated: bool,
}

/// Reduces a `GET /v1/graph` answer to [`Shaped`]. Pure: everything the brain sent beyond a node's
/// label, kind and degree, and an edge's two ends, is dropped here and nowhere else.
pub(crate) fn shape(raw: &Value) -> Shaped {
    let mut out = Shaped {
        truncated: raw.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        ..Shaped::default()
    };
    let mut index: HashMap<&str, usize> = HashMap::new();
    for item in raw.get("nodes").and_then(Value::as_array).into_iter().flatten() {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        if index.contains_key(id) {
            continue;
        }
        if out.nodes.len() >= MAX_NODES {
            out.truncated = true;
            break;
        }
        index.insert(id, out.nodes.len());
        out.nodes.push(Node {
            label: clean(item.get("label").and_then(Value::as_str).unwrap_or(""), MAX_LABEL),
            kind: clean(item.get("kind").and_then(Value::as_str).unwrap_or("topic"), MAX_KIND),
            degree: item.get("degree").and_then(Value::as_u64).unwrap_or(0).min(MAX_DEGREE),
        });
    }
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    for edge in raw.get("edges").and_then(Value::as_array).into_iter().flatten() {
        let source = edge.get("source").and_then(Value::as_str).and_then(|id| index.get(id));
        let target = edge.get("target").and_then(Value::as_str).and_then(|id| index.get(id));
        let (Some(&a), Some(&b)) = (source, target) else { continue };
        if a == b {
            continue;
        }
        let pair = (a.min(b), a.max(b));
        if !seen.insert(pair) {
            continue;
        }
        if out.links.len() >= MAX_LINKS {
            out.truncated = true;
            break;
        }
        out.links.push(pair);
    }
    out
}

/// A string with control characters dropped, cut to `max` characters, and trimmed.
fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The per-process key the structure-only hashes and the cache fingerprint are keyed with. Random
/// at start, never stored or sent, so a hash names nothing outside this process.
static KEY: LazyLock<hmac::Key> = LazyLock::new(|| {
    let mut secret = [0u8; 32];
    SystemRandom::new()
        .fill(&mut secret)
        .expect("the system random source answers");
    hmac::Key::new(hmac::HMAC_SHA256, &secret)
});

fn hex_prefix(data: &[u8], bytes: usize) -> String {
    let tag = hmac::sign(&KEY, data);
    tag.as_ref()[..bytes].iter().map(|b| format!("{b:02x}")).collect()
}

/// The opaque stand-in for a label in structure-only mode: 16 hex characters of its keyed hash.
pub(crate) fn label_hash(label: &str) -> String {
    hex_prefix(label.as_bytes(), 8)
}

/// Which node a repository sits on: the node labelled with its full name, else its name, else its
/// owner (an organisation's own node). Among equals, a project node, then the busiest, then the
/// first in the brain's order. `None` when no label names it.
pub(crate) fn match_repo(nodes: &[Node], repo: &str) -> Option<usize> {
    let repo = repo.to_ascii_lowercase();
    let (owner, name) = repo.split_once('/')?;
    nodes
        .iter()
        .enumerate()
        .filter_map(|(at, node)| {
            let label = node.label.to_ascii_lowercase();
            let score: u8 = if label == repo {
                3
            } else if label == name {
                2
            } else if label == owner {
                1
            } else {
                return None;
            };
            Some(((score, node.kind == "project", node.degree, Reverse(at)), at))
        })
        .max_by_key(|(key, _)| *key)
        .map(|(_, at)| at)
}

/// The node a brain's map is centred on when no colony names one: the busiest, the first on a tie.
pub(crate) fn nest(nodes: &[Node]) -> Option<usize> {
    nodes
        .iter()
        .enumerate()
        .max_by_key(|(at, node)| (node.degree, Reverse(*at)))
        .map(|(at, _)| at)
}

/// The answer the cockpit gets. `structure` replaces every label with its hash; `remote` says the
/// request came through the tunnel (and so is always structure only).
pub(crate) fn answer(shaped: &Shaped, fetched_at: &str, structure: bool, remote: bool, repos: &[String]) -> Value {
    let nodes: Vec<Value> = shaped
        .nodes
        .iter()
        .enumerate()
        .map(|(at, node)| {
            let mut out = json!({ "id": at, "kind": node.kind, "degree": node.degree });
            if structure {
                out["hash"] = json!(label_hash(&node.label));
            } else {
                out["label"] = json!(node.label);
            }
            out
        })
        .collect();
    let links: Vec<[usize; 2]> = shaped.links.iter().map(|&(a, b)| [a, b]).collect();
    let matches: serde_json::Map<String, Value> = repos
        .iter()
        .map(|repo| (repo.clone(), json!(match_repo(&shaped.nodes, repo))))
        .collect();
    json!({
        "state": "connected",
        "structure_only": structure,
        "remote": remote,
        "fetched_at": fetched_at,
        "truncated": shaped.truncated,
        "counts": { "nodes": nodes.len(), "links": links.len() },
        "nodes": nodes,
        "links": links,
        "matches": matches,
        "nest": nest(&shaped.nodes),
    })
}

#[derive(Deserialize)]
pub struct GraphQuery {
    /// `1` replaces labels with hashes. Remote requests are structure only whatever this says.
    structure: Option<String>,
    /// Comma-separated `owner/name` repositories to find on the map.
    repos: Option<String>,
}

/// `GET /api/brain/graph`: the structure of the brain's map, for the cockpit's 3D view. Owner-only
/// (no scoped token has a rule for it), behind `host_guard`'s authentication, with the Origin check
/// here as well: a request that names an Origin must be the cockpit's own.
pub(crate) async fn get_graph(
    State(app): State<Shared>,
    headers: HeaderMap,
    tunnel: Option<Extension<remote::Tunnelled>>,
    Query(query): Query<GraphQuery>,
) -> Result<Json<Value>, AppError> {
    if !same_origin(&headers) {
        return Err(client_error(StatusCode::FORBIDDEN, "cross-origin request rejected"));
    }
    let repos = parse_repos(query.repos.as_deref())?;
    if !admit() {
        return Err(client_error(
            StatusCode::TOO_MANY_REQUESTS,
            "the brain map was asked for too often; wait a minute",
        ));
    }
    let config = &app.cfg.config_dir;
    let Some(tok) = token(config) else {
        return Ok(Json(json!({ "state": "disconnected" })));
    };
    let saved = load(config);
    if saved.auth_error.is_some() {
        return Ok(Json(json!({ "state": "unauthorized" })));
    }
    let key = format!("{}|{}", saved.server_url, hex_prefix(tok.as_bytes(), 8));
    let (shaped, fetched_at) = match graph_for(&saved.server_url, &tok, &key).await {
        Ok(found) => found,
        Err(Refusal::Unauthorized) => {
            if let Err(e) = update(config, |s| s.auth_error = Some("Living Brain refused the token".into())) {
                tracing::warn!(error = %e, "living-brain: the refused token could not be recorded");
            }
            return Ok(Json(json!({ "state": "unauthorized" })));
        }
        Err(Refusal::Upstream(message)) => return Err(client_error(StatusCode::BAD_GATEWAY, &message)),
    };
    let via_tunnel = tunnel.is_some();
    let structure = via_tunnel || query.structure.as_deref() == Some("1");
    Ok(Json(answer(&shaped, &fetched_at, structure, via_tunnel, &repos)))
}

enum Refusal {
    Unauthorized,
    Upstream(String),
}

struct Cached {
    key: String,
    at: Instant,
    fetched_at: String,
    shaped: Arc<Shaped>,
}

static CACHE: LazyLock<Mutex<Option<Cached>>> = LazyLock::new(|| Mutex::new(None));
static RECENT: LazyLock<Mutex<VecDeque<Instant>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

/// The brain's map, from the cache while it is fresh, else read from the brain now.
async fn graph_for(server: &str, tok: &str, key: &str) -> Result<(Arc<Shaped>, String), Refusal> {
    {
        let cache = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hit) = cache.as_ref().filter(|c| c.key == key && c.at.elapsed() < CACHE_TTL) {
            return Ok((hit.shaped.clone(), hit.fetched_at.clone()));
        }
    }
    let raw = Client::new(server, Some(tok.to_owned()))
        .graph(MAX_NODES, MAX_BYTES)
        .await
        .map_err(|e| {
            if e.unauthorized() {
                Refusal::Unauthorized
            } else {
                Refusal::Upstream(e.message)
            }
        })?;
    let shaped = Arc::new(shape(&raw));
    let fetched_at = Utc::now().to_rfc3339();
    *CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Cached {
        key: key.to_owned(),
        at: Instant::now(),
        fetched_at: fetched_at.clone(),
        shaped: shaped.clone(),
    });
    Ok((shaped, fetched_at))
}

/// A sliding one-minute window: whether this request may be answered.
fn admit() -> bool {
    admit_in(
        &mut RECENT.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
        Instant::now(),
    )
}

fn admit_in(recent: &mut VecDeque<Instant>, now: Instant) -> bool {
    while recent
        .front()
        .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
    {
        recent.pop_front();
    }
    if recent.len() >= RATE_PER_MINUTE {
        return false;
    }
    recent.push_back(now);
    true
}

/// An Origin, when one is sent, must be the host the request was made to, over http or https.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else { return false };
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default();
    origin == format!("http://{host}") || origin == format!("https://{host}")
}

fn parse_repos(raw: Option<&str>) -> Result<Vec<String>, AppError> {
    let mut out = Vec::new();
    for item in raw.unwrap_or_default().split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if out.len() >= MAX_REPOS {
            return Err(client_error(StatusCode::BAD_REQUEST, "at most 100 repositories"));
        }
        if !valid_repo(item) {
            return Err(client_error(StatusCode::BAD_REQUEST, "repositories are named owner/name"));
        }
        out.push(item.to_owned());
    }
    Ok(out)
}

/// An `owner/name` as GitHub spells one: letters, digits and `-_.`, never leading with a dot.
fn valid_repo(name: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && !p.starts_with('.')
            && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match name.split_once('/') {
        Some((owner, repo)) => part(owner) && part(repo) && !repo.contains('/'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{temp_root, test_app};
    use axum::{body::Body, extract::Request, routing::get};
    use tower::ServiceExt as _;

    /// A brain map with everything the cockpit must never see, next to what it may.
    fn brain_map() -> Value {
        json!({
            "nodes": [
                { "id": "page:acme-web:ada", "kind": "project", "label": "acme-web", "slug": "acme-web--ada",
                  "scope": "personal", "project": "acme-web", "agent": "claude", "created_at": "2026-01-01T00:00:00Z",
                  "updated_at": "2026-02-01T00:00:00Z", "backlinks": 4, "facts": 2, "degree": 5,
                  "url": "https://app.livingbrain.wiki/p/secret-page" },
                { "id": "page:x:secret", "kind": "decision", "label": "Launch plan for Q4 with a secret codename", "slug": "x",
                  "scope": "org", "project": null, "agent": null, "created_at": null, "updated_at": null,
                  "backlinks": 0, "facts": 9, "degree": 1, "url": null },
                { "id": "session:c1", "kind": "session", "label": "colonizer session c1", "slug": null,
                  "scope": null, "project": "acme", "agent": null, "created_at": null, "updated_at": null,
                  "backlinks": 0, "facts": 0, "degree": 2, "url": null }
            ],
            "edges": [
                { "source": "page:acme-web:ada", "target": "page:x:secret", "kind": "link" },
                { "source": "page:x:secret", "target": "page:acme-web:ada", "kind": "backlink" },
                { "source": "session:c1", "target": "page:acme-web:ada", "kind": "link" },
                { "source": "session:c1", "target": "missing", "kind": "link" }
            ],
            "projects": [{ "id": "acme-web", "name": "acme-web" }],
            "agents": ["claude"],
            "counts": { "nodes": 3 },
            "truncated": false
        })
    }

    #[test]
    fn shaping_keeps_only_label_kind_degree_and_links() {
        let shaped = shape(&brain_map());
        assert_eq!(shaped.nodes.len(), 3);
        assert_eq!(
            shaped.nodes[0],
            Node {
                label: "acme-web".into(),
                kind: "project".into(),
                degree: 5
            }
        );
        // The duplicate-free links between known nodes only: the dangling one is dropped.
        assert_eq!(shaped.links, vec![(0, 1), (0, 2)]);
        let body = answer(&shaped, "2026-10-21T00:00:00Z", false, false, &[]).to_string();
        for leaked in [
            "secret-page",
            "acme-web--ada",
            "\"personal\"",
            "\"facts\":",
            "\"backlinks\":",
            "\"created_at\":",
            "https://",
            "\"slug\":",
            "\"url\":",
            "\"scope\":",
            "\"agent\":",
            "\"project\":",
            "\"updated_at\":",
        ] {
            assert!(!body.contains(leaked), "{leaked} leaked: {body}");
        }
        let node_keys: HashSet<String> = answer(&shaped, "t", false, false, &[])["nodes"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            node_keys,
            ["id", "kind", "degree", "label"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn structure_only_replaces_every_label_with_a_keyed_hash() {
        let shaped = shape(&brain_map());
        let out = answer(&shaped, "t", true, false, &["acme/web".into()]);
        let text = out.to_string();
        assert!(
            !text.contains("acme-web\""),
            "a label reached a structure-only answer: {text}"
        );
        assert!(!text.contains("secret codename"), "{text}");
        assert!(!text.contains("Launch plan"), "{text}");
        assert_eq!(out["structure_only"], true);
        let hash = out["nodes"][0]["hash"].as_str().unwrap();
        assert_eq!(hash.len(), 16);
        assert_eq!(
            hash,
            label_hash("acme-web"),
            "the same label hashes the same within a process"
        );
        assert_ne!(label_hash("acme-web"), label_hash("acme-wbe"));
        assert!(out["nodes"][0].get("label").is_none());
    }

    #[test]
    fn repositories_match_their_nodes_by_name_org_or_project() {
        let shaped = shape(&brain_map());
        // Full name beats name beats owner; the project node is the one labelled acme-web.
        assert_eq!(match_repo(&shaped.nodes, "acme/acme-web"), Some(0));
        assert_eq!(match_repo(&shaped.nodes, "other/acme-web"), Some(0));
        assert_eq!(
            match_repo(&shaped.nodes, "acme/unknown"),
            None,
            "the owner is a label too: acme is no node here"
        );
        assert_eq!(match_repo(&shaped.nodes, "no-slash"), None);
        assert_eq!(nest(&shaped.nodes), Some(0), "the busiest node is the nest");
        assert_eq!(nest(&[]), None);
    }

    #[test]
    fn repository_names_are_checked() {
        assert!(parse_repos(Some("acme/web, acme/api")).is_ok());
        assert!(parse_repos(Some("acme/web/extra")).is_err());
        assert!(parse_repos(Some("../etc")).is_err());
        assert!(parse_repos(Some(&(0..101).map(|i| format!("o/r{i}")).collect::<Vec<_>>().join(","))).is_err());
        assert!(parse_repos(None).unwrap().is_empty());
    }

    #[test]
    fn a_foreign_origin_is_refused_and_the_cockpits_own_is_not() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:7878".parse().unwrap());
        assert!(same_origin(&headers), "no Origin: a non-browser caller");
        headers.insert(header::ORIGIN, "http://127.0.0.1:7878".parse().unwrap());
        assert!(same_origin(&headers));
        headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!same_origin(&headers));
        headers.insert(header::ORIGIN, "null".parse().unwrap());
        assert!(!same_origin(&headers));
    }

    #[test]
    fn the_rate_window_admits_sixty_a_minute_and_then_refuses() {
        let start = Instant::now();
        let mut recent = VecDeque::new();
        assert!((0..RATE_PER_MINUTE).all(|_| admit_in(&mut recent, start)));
        assert!(!admit_in(&mut recent, start), "the 61st in a minute is refused");
        assert!(admit_in(&mut recent, start + Duration::from_secs(61)), "the window slides");
    }

    /// A fake brain: `GET /v1/graph` answers the map only to the one token, and counts its reads.
    async fn fake_brain(token: &'static str) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = reads.clone();
        let app = axum::Router::new().route(
            "/v1/graph",
            get(move |headers: HeaderMap| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let presented = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
                    if presented != format!("Bearer {token}") {
                        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" })));
                    }
                    (StatusCode::OK, Json(brain_map()))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), reads)
    }

    fn connect(app: &Shared, server: &str, token: &str) {
        crate::util::write_secret(&super::super::token_file(&app.cfg.config_dir), token).unwrap();
        update(&app.cfg.config_dir, |s| {
            s.server_url = server.to_owned();
            s.token_prefix = Some("lbp_tok1".into());
            s.auth_error = None;
        })
        .unwrap();
    }

    async fn get_json(app: &Shared, uri: &str, extra: Vec<(header::HeaderName, String)>) -> (StatusCode, String) {
        let mut req = Request::builder()
            .method("GET")
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7878");
        for (name, value) in extra {
            req = req.header(name, value);
        }
        let res = crate::server::router(app)
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn the_route_wants_the_cockpit_token_and_never_echoes_the_brain_token() {
        let root = temp_root();
        let app = test_app(&root);
        let secret = "lbp_tok1_secretpart";
        let (server, reads) = fake_brain(secret).await;

        let (status, _) = get_json(&app, "/api/brain/graph", vec![]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "no credential, no map");

        let bearer = vec![(header::AUTHORIZATION, format!("Bearer {}", app.api_token))];
        let (status, body) = get_json(&app, "/api/brain/graph", bearer.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"disconnected\""), "{body}");

        connect(&app, &server, secret);
        let (status, body) = get_json(&app, "/api/brain/graph", bearer.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let answer: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(answer["state"], "connected");
        assert_eq!(answer["nodes"][0]["label"], "acme-web");
        assert!(
            !body.contains(secret) && !body.contains("secretpart"),
            "the brain token reached the cockpit"
        );

        // The second read within the minute is the cache's: the brain is asked once.
        let (status, _) = get_json(&app, "/api/brain/graph?structure=1", bearer.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);

        let (status, body) = get_json(&app, "/api/brain/graph?structure=1", bearer.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"hash\"") && !body.contains("acme-web"), "{body}");

        let mut foreign = bearer.clone();
        foreign.push((header::ORIGIN, "https://evil.example".into()));
        let (status, _) = get_json(&app, "/api/brain/graph", foreign).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a foreign Origin is refused");

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_refused_brain_token_marks_the_connection_and_names_no_secret() {
        let root = temp_root();
        let app = test_app(&root);
        let (server, _) = fake_brain("lbp_other_token").await;
        connect(&app, &server, "lbp_tok2_wrongpart");
        let bearer = vec![(header::AUTHORIZATION, format!("Bearer {}", app.api_token))];
        let (status, body) = get_json(&app, "/api/brain/graph", bearer).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"unauthorized\""), "{body}");
        assert!(!body.contains("wrongpart"), "{body}");
        assert_eq!(
            load(&app.cfg.config_dir).auth_error.as_deref(),
            Some("Living Brain refused the token")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_tunnelled_request_is_structure_only_whatever_it_asks() {
        let root = temp_root();
        let app = test_app(&root);
        let (server, _) = fake_brain("lbp_tok3_remotepart").await;
        connect(&app, &server, "lbp_tok3_remotepart");
        let tunnel = Some(Extension(remote::Tunnelled {
            host: "example.invalid".into(),
        }));
        let Json(out) = get_graph(
            State(app.clone()),
            HeaderMap::new(),
            tunnel,
            Query(GraphQuery {
                structure: Some("0".into()),
                repos: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(out["remote"], true);
        assert_eq!(out["structure_only"], true);
        assert!(out["nodes"][0].get("label").is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
