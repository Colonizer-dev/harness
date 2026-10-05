//! The Unified Harness Protocol surface (docs/protocol.md §7): the spec version every `/uhp/…`
//! reply advertises, version negotiation, the §7.7 error envelope both route families answer
//! with, and the read-side core routes that belong to no other feature (issue #650) — discovery,
//! harnesses, models and the single colony. The routes that do belong to a feature live with it
//! (`sessions/files.rs` has the §7.2 session page and the §7.5 artifacts) and share the layer and
//! the refusals kept here. Creating and continuing responses, SSE streaming and cancellation live
//! in `uhp_responses.rs`, merged into the same layer.

use axum::{
    Json,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse as _, Response},
    routing::get,
};
use serde_json::{Value, json};

use crate::{App, Shared, api_tokens, modules::AgentModule};

/// The spec version §7.1 makes every `/uhp/v1/…` reply send.
pub(crate) const VERSION: &str = "2026-09-12";

/// The request header a client sets to ask for UHP shapes, and the reply header the `/uhp`
/// surface always sends (§7.1).
pub(crate) const VERSION_HEADER: HeaderName = HeaderName::from_static("uhp-version");

/// Stamps a reply with [`VERSION`]: what every `/uhp/…` route wraps its answer in.
pub(crate) fn stamped(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(VERSION_HEADER, HeaderValue::from_static(VERSION));
    response
}

/// Whether a request asked an `/api/…` route for UHP shapes (§7.1): errors then come in the
/// §7.7 envelope instead of Colonizer's own.
fn speaks_uhp(headers: &HeaderMap) -> bool {
    headers.contains_key(VERSION_HEADER)
}

/// The §7.7 envelope: the shape OpenAI's Responses API established, with the code in UHP's
/// vocabulary where it has one. A caller's mistake — a bad id, a bad name, an oversize read —
/// is the request-error type; a failure on our side is the server one.
fn envelope(status: StatusCode, code: &str, message: String, detail: Value) -> Response {
    let kind = if status.is_server_error() {
        "server_error"
    } else {
        "invalid_request_error"
    };
    typed_envelope(status, kind, code, message, detail)
}

/// The envelope with its `type` named by the caller: the credential refusals are
/// `authentication_error` and `permission_error` rather than a request error (§7.7).
pub(crate) fn typed_envelope(status: StatusCode, kind: &str, code: &str, message: String, detail: Value) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "type": kind,
                "code": code,
                "message": message,
                "param": Value::Null,
                "detail": detail,
            }
        })),
    )
        .into_response()
}

/// Colonizer's own error body with the code as a sibling (§7.7): what an `/api/…` route answers
/// when the request does not speak UHP. The detail is envelope-only and is dropped here.
fn coded(status: StatusCode, code: &str, message: String) -> Response {
    (status, Json(json!({"error": message, "code": code}))).into_response()
}

/// A `/uhp/…` route's refusal: always the §7.7 envelope, with `detail` riding along when the
/// code carries one (`file_too_large`'s `max_bytes`, §7.5).
pub(crate) fn uhp_error(status: StatusCode, code: &str, message: impl std::fmt::Display, detail: Option<Value>) -> Response {
    envelope(status, code, message.to_string(), detail.unwrap_or(Value::Null))
}

/// An `/api/…` route's refusal: the §7.7 envelope when the request sent `UHP-Version`,
/// Colonizer's string error with a `code` sibling otherwise (§7.7). The detail is envelope-only
/// and is dropped with the envelope.
pub(crate) fn api_error(
    headers: &HeaderMap,
    status: StatusCode,
    code: &str,
    message: impl std::fmt::Display,
    detail: Option<Value>,
) -> Response {
    if speaks_uhp(headers) {
        envelope(status, code, message.to_string(), detail.unwrap_or(Value::Null))
    } else {
        coded(status, code, message.to_string())
    }
}

/// The one refusal both routes of a `/api`/`/uhp` pair answer with: the shared helpers take the
/// surface from their caller and land here.
pub(crate) fn error_for(
    uhp: bool,
    headers: &HeaderMap,
    status: StatusCode,
    code: &str,
    message: impl std::fmt::Display,
    detail: Option<Value>,
) -> Response {
    if uhp {
        uhp_error(status, code, message, detail)
    } else {
        api_error(headers, status, code, message, detail)
    }
}

/// The discovery route: the one `/uhp` path served before any credential check, because a
/// client has to learn this is a UHP server before it can decide what credential to present.
/// Kept in step with the literal in [`routes`] by a test below.
pub(crate) const DISCOVERY_PATH: &str = "/uhp/v1/uhp";

/// Whether a request path belongs to the UHP surface. Segment-aware like the `/api` fence in
/// `server.rs`, so a cockpit path that merely begins with the same letters is not caught.
pub(crate) fn is_uhp(path: &str) -> bool {
    path == "/uhp" || path.starts_with("/uhp/")
}

/// The routes this module serves (issue #650). `server::api_routes` merges them flat, behind the
/// activity log's route layer and `host_guard`, so `api_not_found` keeps seeing the matched path.
/// The session detail registers `{id}` like `sessions/files.rs`'s `/uhp/v1/sessions/{id}/files…`
/// — the router refuses two names for one segment.
pub(crate) fn routes() -> axum::Router<Shared> {
    axum::Router::new()
        .route("/uhp/v1/uhp", get(discovery).fallback(method_not_allowed))
        .route("/uhp/v1/harnesses", get(harnesses).fallback(method_not_allowed))
        .route("/uhp/v1/harnesses/{harness_id}", get(harness).fallback(method_not_allowed))
        .route("/uhp/v1/models", get(models).fallback(method_not_allowed))
        .route("/uhp/v1/sessions/{id}", get(session).fallback(method_not_allowed))
        .merge(crate::uhp_responses::routes())
        .route_layer(middleware::from_fn(negotiate))
}

// ---------------------------------------------------------------------------
// Version negotiation and the refusals every /uhp route shares
// ---------------------------------------------------------------------------

/// Version negotiation (§7.1), the route layer over every served `/uhp` route — this module's and
/// the feature routes' alike: a request naming a version this build does not speak is refused
/// with the list of the ones it does, never silently served a different contract. A request
/// naming none gets the default. Whatever comes back is stamped with [`VERSION`].
pub(crate) async fn negotiate(req: Request, next: Next) -> Response {
    let asked = req.headers().get(VERSION_HEADER).map(|v| v.to_str().unwrap_or(""));
    if let Some(asked) = asked
        && asked != VERSION
    {
        return stamped(uhp_error(
            StatusCode::BAD_REQUEST,
            "unsupported_protocol_version",
            format!("this server speaks UHP {VERSION} only"),
            Some(json!({ "supported": [VERSION] })),
        ));
    }
    stamped(next.run(req).await)
}

/// The 405 a served `/uhp` route answers a method it does not have — what a `POST
/// /uhp/v1/harnesses` reads (the suite's F-02): the envelope, not axum's empty answer. axum appends
/// the `Allow` header naming the methods the route does serve, and [`negotiate`] the version.
pub(crate) async fn method_not_allowed() -> Response {
    uhp_error(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "this UHP route does not serve that method",
        None,
    )
}

/// The 401 every `/uhp` path but discovery answers without — or with a wrong — credential: the
/// envelope, not the cockpit's sign-in page, so a protocol client can read the refusal.
pub(crate) fn unauthenticated() -> Response {
    stamped(typed_envelope(
        StatusCode::UNAUTHORIZED,
        "authentication_error",
        "invalid_credential",
        "a valid cockpit API token is required for this route".into(),
        Value::Null,
    ))
}

/// A scoped token that authenticated but may not touch the route: the verdicts `Deny` serves the
/// `/api` surface, in the envelope — the out-of-limits colony still reads as an unknown one.
pub(crate) fn denied(deny: api_tokens::Deny) -> Response {
    stamped(match deny {
        api_tokens::Deny::Forbidden(message) => typed_envelope(
            StatusCode::FORBIDDEN,
            "permission_error",
            "insufficient_scope",
            message,
            Value::Null,
        ),
        api_tokens::Deny::NoSession => uhp_error(StatusCode::NOT_FOUND, "session_not_found", "no such session", None),
        api_tokens::Deny::NoResponse => uhp_error(StatusCode::NOT_FOUND, "response_not_found", "no such response", None),
        api_tokens::Deny::NoMap => uhp_error(StatusCode::NOT_FOUND, "not_found", "no such UHP route", None),
    })
}

/// The JSON 404 every `/uhp` path no route claims answers with (`#641`'s answer, carried over to
/// the protocol's paths): a miss must read as an error, never as the cockpit's page. A miss under
/// `/responses` (a path no response route serves, `uhp_responses.rs`) names
/// the response, one under `/containers` (§7.5's artifact reads) the file, and anything else the
/// route. The prefixes match on segment boundaries (`/uhp/v1/responsesX` is a route miss), and the
/// path itself is never echoed: a probe may carry traversal segments.
pub(crate) fn unmatched(path: &str) -> Response {
    let code = if under(path, "/uhp/v1/responses") {
        "response_not_found"
    } else if under(path, "/uhp/v1/containers") {
        "file_not_found"
    } else {
        "not_found"
    };
    stamped(uhp_error(StatusCode::NOT_FOUND, code, "no such UHP route", None))
}

/// Whether `path` is `prefix` itself or a route under it.
fn under(path: &str, prefix: &str) -> bool {
    path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// `GET /uhp/v1/uhp` — the discovery document, served without credentials (the suite's D-02
/// checks exactly that). The capabilities are the honest list of what the surface serves today:
/// the session page, the artifact reads, streaming and cancellation are on the wire (§7.2–§7.6),
/// while input files are not (§7.5) — a false capability reads
/// as "not served", which is true here.
async fn discovery() -> Json<Value> {
    Json(json!({
        "object": "uhp.discovery",
        "protocol": "uhp",
        "versions": [VERSION],
        "default_version": VERSION,
        "conformance_class": "core",
        "capabilities": {
            "sessions": true,
            "session_listing": true,
            "streaming": true,
            "cancellation": true,
            "files_input": false,
            "files_output": true,
            "harness_management": false,
            "session_sharing": false,
            "plugins": false,
        },
        "implementation": { "name": "colonizer", "version": env!("CARGO_PKG_VERSION") },
    }))
}

// ---------------------------------------------------------------------------
// Harnesses and models
// ---------------------------------------------------------------------------

/// `GET /uhp/v1/harnesses` — the installed agent modules as harnesses. Empty when the mothership
/// runs without its bundled assets: nothing could launch, so there is nothing to list.
async fn harnesses(State(app): State<Shared>) -> Json<Value> {
    let disabled = disabled_pick(&app).await;
    let harnesses: Vec<Value> = app
        .agents
        .iter()
        .filter(|module| disabled.as_deref() != Some(module.id.as_str()))
        .map(harness_json)
        .collect();
    Json(json!({ "object": "list", "harnesses": harnesses }))
}

/// `GET /uhp/v1/harnesses/{harness_id}` — one harness. An unknown id, a missing `chrn_` prefix
/// and a switched-off module all read the same `harness_not_found`.
async fn harness(State(app): State<Shared>, Path(harness_id): Path<String>) -> Response {
    let found = harness_id
        .strip_prefix("chrn_")
        .filter(|id| !id.is_empty())
        .and_then(|id| app.agents.iter().find(|module| module.id == id));
    let disabled = disabled_pick(&app).await;
    match found {
        Some(module) if disabled.as_deref() != Some(module.id.as_str()) => Json(harness_json(module)).into_response(),
        _ => uhp_error(StatusCode::NOT_FOUND, "harness_not_found", "no such harness", None),
    }
}

/// The wire shape of one harness (schema `Harness`): `base` is the agent module id a launch names,
/// `defaultModel` the manifest's declared default when it declares one. Nothing else is invented.
fn harness_json(module: &AgentModule) -> Value {
    let mut harness = json!({
        "object": "harness",
        "id": format!("chrn_{}", module.id),
        "name": module.name,
        "base": module.id,
    });
    if let Some(default) = module
        .schema
        .get("properties")
        .and_then(|properties| properties.get("model"))
        .and_then(|model| model.get("default"))
        .and_then(Value::as_str)
        .filter(|default| !default.is_empty())
    {
        harness["defaultModel"] = json!(default);
    }
    harness
}

/// The install's agent-module pick while it is switched off in Settings, if it is: a disabled pick
/// cannot launch a colony, so it reads as absent. Other installed modules stay listed — an org's
/// `agent.module` override can still launch on them.
async fn disabled_pick(app: &App) -> Option<String> {
    let modules = app.modules.read().await;
    (!modules.agent.enabled).then(|| modules.agent.provider.clone())
}

/// `GET /uhp/v1/models` — the model catalogue. Colonizer's model lists live per provider in the
/// gateway, with availability a function of the stored keys, and no agent module declares a list
/// of its own; the empty catalogue says so rather than inventing entries.
async fn models() -> Json<Value> {
    Json(json!({ "backends": {} }))
}

// ---------------------------------------------------------------------------
// The single colony
// ---------------------------------------------------------------------------

/// `GET /uhp/v1/sessions/{id}` — one colony, in exactly the shape a row of `GET /uhp/v1/sessions`
/// has (the colony with its live activity), so a client reads the list and the detail alike. An id
/// outside a scoped token's limits reads as an unknown one: `authorize` already answers it 404
/// before the handler runs, and the handler repeats the hiding in case a caller reaches it another
/// way.
async fn session(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<api_tokens::ScopedToken>>,
    Path(id): Path<String>,
) -> Response {
    match app.session(&id).await {
        Some(session) if scoped.as_ref().is_none_or(|token| token.covers(&session.org, &session.repo)) => {
            Json(crate::sessions::with_activity(&app, session).await).into_response()
        }
        _ => uhp_error(StatusCode::NOT_FOUND, "session_not_found", "no such session", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_tokens::NewToken;
    use crate::server::router;
    use crate::sessions::SessionStatus;
    use crate::sessions::tests::colony;
    use crate::tests::{temp_root, test_app, test_app_with_agents};
    use axum::body::Body;
    use axum::http::{Method, header};
    use tower::ServiceExt as _;

    /// The discovery literal and the path `host_guard` exempts must stay the same string: the
    /// route table reads the literal in [`routes`], the guard reads the constant.
    #[test]
    fn the_discovery_route_constant_names_the_served_route() {
        assert_eq!(DISCOVERY_PATH, "/uhp/v1/uhp");
        let source = include_str!("uhp.rs");
        assert!(source.contains(&format!(".route(\"{DISCOVERY_PATH}\", get(discovery)")));
    }

    fn request(method: Method, uri: &str, headers: Vec<(HeaderName, String)>) -> Request {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7878");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn get(uri: &str, headers: Vec<(HeaderName, String)>) -> Request {
        request(Method::GET, uri, headers)
    }

    fn bearer(app: &Shared) -> (HeaderName, String) {
        (header::AUTHORIZATION, format!("Bearer {}", app.api_token))
    }

    fn version(v: &str) -> (HeaderName, String) {
        (VERSION_HEADER, v.into())
    }

    async fn body_json(res: Response) -> Value {
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn error_of(body: &Value) -> serde_json::Map<String, Value> {
        body["error"].as_object().unwrap().clone()
    }

    fn stamped_with_version(res: &Response) -> bool {
        res.headers().get(VERSION_HEADER).map(|v| v.as_bytes()) == Some(VERSION.as_bytes())
    }

    /// A module manifest with a `model` setting default when one is given.
    fn agent(id: &str, name: &str, model_default: Option<&str>) -> AgentModule {
        let mut schema = json!({"type": "object", "properties": {}});
        if let Some(default) = model_default {
            schema["properties"]["model"] = json!({"type": "string", "default": default});
        }
        AgentModule::test(id).name(name).schema(schema)
    }

    async fn scoped_read_token(app: &Shared, orgs: &[&str]) -> String {
        app.api_tokens
            .create(NewToken {
                name: "ci".into(),
                scope: "read".into(),
                orgs: orgs.iter().map(|o| o.to_string()).collect(),
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            })
            .await
            .unwrap()
            .token
    }

    async fn push_colony(app: &Shared, id: &str, org: &str, status: SessionStatus) {
        let mut session = colony(org, status);
        session.id = id.into();
        session.agent = "claude-code".into();
        app.sessions.write().await.push(session);
    }

    #[tokio::test]
    async fn discovery_answers_without_credentials_and_states_the_honest_surface() {
        let root = temp_root();
        let app = test_app(&root);
        let res = router(&app).oneshot(get(DISCOVERY_PATH, vec![])).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(stamped_with_version(&res), "every /uhp answer names its version");
        let body = body_json(res).await;
        assert_eq!(body["object"], "uhp.discovery");
        assert_eq!(body["protocol"], "uhp");
        assert_eq!(body["versions"], json!([VERSION]));
        assert_eq!(body["default_version"], VERSION);
        assert_eq!(body["conformance_class"], "core");
        assert_eq!(body["implementation"]["name"], "colonizer");
        // What is served, not what is planned: the artifact reads, streaming and cancellation
        // are on the wire, input files are not.
        for (cap, want) in [
            ("sessions", true),
            ("session_listing", true),
            ("streaming", true),
            ("cancellation", true),
            ("files_input", false),
            ("files_output", true),
            ("harness_management", false),
            ("session_sharing", false),
            ("plugins", false),
        ] {
            assert_eq!(body["capabilities"][cap], want, "capability {cap}");
        }

        // HEAD probes the same open door; a scoped token is not refused on a public route.
        let res = router(&app)
            .oneshot(request(Method::HEAD, DISCOVERY_PATH, vec![]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let token = scoped_read_token(&app, &["acme"]).await;
        let res = router(&app)
            .oneshot(get(DISCOVERY_PATH, vec![(header::AUTHORIZATION, format!("Bearer {token}"))]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // Only discovery is open: its neighbours are behind the wall.
        let res = router(&app)
            .oneshot(request(Method::HEAD, "/uhp/v1/harnesses", vec![]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unsupported_version_is_refused_with_the_supported_one() {
        let root = temp_root();
        let app = test_app(&root);
        let res = router(&app)
            .oneshot(get(DISCOVERY_PATH, vec![version("1999-01-01")]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(stamped_with_version(&res), "even the refusal names the version it speaks");
        let err = error_of(&body_json(res).await);
        assert_eq!(err["type"], "invalid_request_error");
        assert_eq!(err["code"], "unsupported_protocol_version");
        assert_eq!(err["detail"]["supported"], json!([VERSION]));

        // The version it does speak is served.
        let res = router(&app)
            .oneshot(get(DISCOVERY_PATH, vec![version(VERSION)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Negotiation covers the feature routes #651 put under `/uhp` too — the session page and the
    /// artifact reads — not only this module's: one layer, every served route.
    #[tokio::test]
    async fn version_negotiation_covers_the_session_page_and_the_artifact_routes() {
        let root = temp_root();
        let app = test_app(&root);
        push_colony(&app, "c1", "acme", SessionStatus::Stopped).await;
        for uri in [
            "/uhp/v1/sessions",
            "/uhp/v1/sessions/c1",
            "/uhp/v1/sessions/c1/files",
            "/uhp/v1/containers/cntr_c1/files/x.txt/content",
        ] {
            let res = router(&app)
                .oneshot(get(uri, vec![bearer(&app), version("2020-01-01")]))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert!(stamped_with_version(&res), "{uri}");
            assert_eq!(
                error_of(&body_json(res).await)["code"],
                "unsupported_protocol_version",
                "{uri}"
            );
        }
        let res = router(&app)
            .oneshot(get("/uhp/v1/sessions", vec![bearer(&app), version(VERSION)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(stamped_with_version(&res));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn harness_routes_sit_behind_the_token_wall_in_uhp_envelopes() {
        let root = temp_root();
        let app = test_app(&root);
        for headers in [vec![], vec![(header::AUTHORIZATION, "Bearer not-a-token".into())]] {
            let res = router(&app).oneshot(get("/uhp/v1/harnesses", headers)).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            assert!(stamped_with_version(&res));
            let err = error_of(&body_json(res).await);
            assert_eq!(err["type"], "authentication_error");
            assert_eq!(err["code"], "invalid_credential");
            assert!(err["message"].is_string());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn harnesses_list_the_installed_modules_and_unknown_ones_read_the_same() {
        let root = temp_root();
        let app = test_app_with_agents(
            &root,
            vec![
                agent("claude-code", "Claude Code", None),
                agent("grok-build", "Grok Build", Some("xai-grok/grok-4.5")),
            ],
            |_| {},
        );
        let router = router(&app);
        let res = router
            .clone()
            .oneshot(get("/uhp/v1/harnesses", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(stamped_with_version(&res));
        let body = body_json(res).await;
        assert_eq!(body["object"], "list");
        let listed = body["harnesses"].as_array().unwrap();
        assert_eq!(listed.len(), 2, "{body}");
        assert_eq!(listed[0]["id"], "chrn_claude-code");
        assert_eq!(listed[0]["base"], "claude-code");
        assert_eq!(listed[0]["name"], "Claude Code");
        assert_eq!(listed[0]["object"], "harness");
        assert!(
            listed[0].get("defaultModel").is_none(),
            "no default is invented where the manifest declares none"
        );
        assert_eq!(listed[1]["defaultModel"], "xai-grok/grok-4.5");

        let res = router
            .clone()
            .oneshot(get("/uhp/v1/harnesses/chrn_grok-build", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["id"], "chrn_grok-build");
        for id in ["chrn_nosuch", "grok-build", "chrn_"] {
            let res = router
                .clone()
                .oneshot(get(&format!("/uhp/v1/harnesses/{id}"), vec![bearer(&app)]))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{id}");
            let err = error_of(&body_json(res).await);
            assert_eq!(err["code"], "harness_not_found", "{id}");
            assert_eq!(err["type"], "invalid_request_error", "{id}");
        }

        let res = router.oneshot(get("/uhp/v1/models", vec![bearer(&app)])).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await, json!({"backends": {}}));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_switched_off_pick_is_no_harness_at_all() {
        let root = temp_root();
        let app = test_app_with_agents(&root, vec![agent("claude-code", "Claude Code", None)], |_| {});
        {
            let mut modules = app.modules.write().await;
            modules.agent.enabled = false;
            modules.agent.provider = "claude-code".into();
        }
        let router = router(&app);
        let res = router
            .clone()
            .oneshot(get("/uhp/v1/harnesses", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(body_json(res).await["harnesses"], json!([]));
        let res = router
            .oneshot(get("/uhp/v1/harnesses/chrn_claude-code", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(error_of(&body_json(res).await)["code"], "harness_not_found");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Protocol paths no route claims are 404 envelopes, not pages: a missing response names the
    /// response, a miss under `/containers` the file, and a traversal probe is refused without
    /// echoing what it carried. The prefix reads on segment boundaries.
    #[tokio::test]
    async fn unmatched_uhp_paths_answer_envelopes_and_never_serve_a_page() {
        let root = temp_root();
        let app = test_app(&root);
        let router = router(&app);
        for (probe, code) in [
            ("/uhp/v1/responses/resp_nosuch", "response_not_found"),
            ("/uhp/v1/responses/resp_nosuch/nosuch", "response_not_found"),
            ("/uhp/v1/responsesX", "not_found"),
            ("/uhp/v1/containers", "file_not_found"),
            ("/uhp/v1/containersX", "not_found"),
        ] {
            let res = router.clone().oneshot(get(probe, vec![bearer(&app)])).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{probe}");
            assert!(stamped_with_version(&res), "{probe}");
            assert_eq!(error_of(&body_json(res).await)["code"], code, "{probe}");
        }

        for probe in [
            "/uhp/v1/containers/cntr_x/files/../../etc/passwd/content",
            "/uhp/v1/containers/cntr_x/files/..%2f..%2fetc%2fpasswd/content",
        ] {
            let res = router.clone().oneshot(get(probe, vec![bearer(&app)])).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{probe}");
            assert_eq!(
                res.headers().get(header::CONTENT_TYPE).unwrap(),
                "application/json",
                "{probe}"
            );
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
            let text = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(!text.contains("root:"), "{probe} served a file: {text}");
            assert!(!text.contains("etc/passwd"), "{probe} echoed the path");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// A method a served route lacks is the envelope too — this module's routes and the ones #651
    /// registered under `/uhp` alike — with `Allow` naming what is served and `UHP-Version` on it.
    #[tokio::test]
    async fn a_method_a_route_lacks_is_an_envelope_too() {
        let root = temp_root();
        let app = test_app(&root);
        let router = router(&app);
        for (method, uri) in [
            (Method::POST, DISCOVERY_PATH),
            (Method::POST, "/uhp/v1/harnesses"),
            (Method::DELETE, "/uhp/v1/sessions/s1"),
            (Method::PUT, "/uhp/v1/models"),
            (Method::POST, "/uhp/v1/sessions"),
            (Method::DELETE, "/uhp/v1/sessions/s1/files"),
            (Method::POST, "/uhp/v1/containers/cntr_s1/files/a.txt/content"),
        ] {
            let res = router
                .clone()
                .oneshot(request(method.clone(), uri, vec![bearer(&app)]))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
            assert!(stamped_with_version(&res), "{method} {uri}");
            assert_eq!(res.headers().get(header::ALLOW).unwrap(), "GET,HEAD", "{method} {uri}");
            let err = error_of(&body_json(res).await);
            assert_eq!(err["type"], "invalid_request_error", "{method} {uri}");
            assert_eq!(err["code"], "method_not_allowed", "{method} {uri}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// The detail and a row of the page are one shape (the colony with its live activity), and
    /// the page walks by `next_cursor` the way #651 defined it: the last row's id.
    #[tokio::test]
    async fn the_session_detail_reads_like_a_row_of_the_page() {
        let root = temp_root();
        let app = test_app(&root);
        push_colony(&app, "old", "acme", SessionStatus::Stopped).await;
        push_colony(&app, "mid", "acme", SessionStatus::Running).await;
        push_colony(&app, "new", "acme", SessionStatus::Queued).await;
        let router = router(&app);

        let res = router
            .clone()
            .oneshot(get("/uhp/v1/sessions?limit=1", vec![bearer(&app)]))
            .await
            .unwrap();
        let body = body_json(res).await;
        let row = body["sessions"][0].clone();
        assert_eq!(row["id"], "new", "newest first: {body}");
        assert_eq!(body["next_cursor"], "new");

        let res = router
            .clone()
            .oneshot(get("/uhp/v1/sessions/new", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(stamped_with_version(&res));
        assert_eq!(body_json(res).await, row, "the detail is the row");

        let res = router
            .clone()
            .oneshot(get("/uhp/v1/sessions/nosuch", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let err = error_of(&body_json(res).await);
        assert_eq!(err["code"], "session_not_found");
        assert_eq!(err["type"], "invalid_request_error");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #650: a scoped `read` token watches through UHP exactly as through `/api` — the list
    /// is filtered to its orgs, a colony outside them reads as unknown (in the envelope, on the
    /// detail and on #651's artifact routes alike), never as a 403, and a path off the allowlist is
    /// the envelope's permission error.
    #[tokio::test]
    async fn a_scoped_read_token_is_held_to_its_limits() {
        let root = temp_root();
        let app = test_app(&root);
        let token = scoped_read_token(&app, &["acme"]).await;
        push_colony(&app, "mine", "acme", SessionStatus::Running).await;
        push_colony(&app, "theirs", "other", SessionStatus::Running).await;
        let router = router(&app);
        let scoped = || vec![(header::AUTHORIZATION, format!("Bearer {token}"))];

        let res = router.clone().oneshot(get("/uhp/v1/sessions", scoped())).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        let listed = body["sessions"].as_array().unwrap();
        assert_eq!(listed.len(), 1, "{body}");
        assert_eq!(listed[0]["id"], "mine");

        for uri in [
            "/uhp/v1/sessions/theirs",
            "/uhp/v1/sessions/theirs/files",
            "/uhp/v1/sessions/theirs/files/archive",
            "/uhp/v1/containers/cntr_theirs/files/a.txt/content",
        ] {
            let res = router.clone().oneshot(get(uri, scoped())).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(stamped_with_version(&res), "{uri}");
            let err = error_of(&body_json(res).await);
            assert_eq!(err["code"], "session_not_found", "{uri}");
            assert_eq!(err["type"], "invalid_request_error", "{uri}");
        }
        let res = router.clone().oneshot(get("/uhp/v1/sessions/mine", scoped())).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["id"], "mine");
        for uri in ["/uhp/v1/harnesses", "/uhp/v1/models"] {
            let res = router.clone().oneshot(get(uri, scoped())).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri}");
        }

        // `read` covers every served route here, so the permission error needs a path the
        // allowlist does not list: a scoped token may not probe the surface's corners either.
        let res = router.oneshot(get("/uhp/v1/does-not-exist", scoped())).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert!(stamped_with_version(&res));
        let err = error_of(&body_json(res).await);
        assert_eq!(err["type"], "permission_error");
        assert_eq!(err["code"], "insufficient_scope");
        let _ = std::fs::remove_dir_all(root);
    }
}
