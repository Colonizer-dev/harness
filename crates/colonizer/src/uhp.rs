//! The UHP read surface (docs/protocol.md §7, issue #650): the [Unified Harness
//! Protocol](https://unifiedharnessprotocol.org/)'s names for what the mothership already serves —
//! discovery, harnesses, models and session listing under `/uhp/v1`, answered in UHP shapes with
//! `UHP-Version` on every response. Creating and continuing responses, SSE streaming, cancellation
//! and files are the follow-up; the discovery document says so in its capabilities rather than
//! pretending, which is why the core class reads as not conformant until they land.

use axum::{
    Json,
    extract::{Path, Query, Request, State},
    http::{HeaderName, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{App, Shared, api_tokens, modules::AgentModule, sessions::Session};

/// The one protocol version this surface speaks (docs/protocol.md §7).
pub(crate) const PROTOCOL_VERSION: &str = "2026-09-12";

/// The response header every `/uhp` answer carries, errors included (spec: lifecycle §1) — a
/// client must be able to tell which contract an answer speaks.
const VERSION_HEADER: HeaderName = HeaderName::from_static("uhp-version");

/// The discovery route. The one `/uhp` path served before any credential check: a client has to
/// learn this is a UHP server before it can decide what credential to present. Kept in step with
/// the literal in [`routes`] by a test below.
pub(crate) const DISCOVERY_PATH: &str = "/uhp/v1/uhp";

/// Page size when a list request names no `limit`.
const DEFAULT_LIMIT: usize = 20;
/// The largest page a list request may ask for.
const MAX_LIMIT: usize = 100;

/// Whether a request path belongs to the UHP surface. Segment-aware like the `/api` fence in
/// `server.rs`, so a cockpit path that merely begins with the same letters is not caught.
pub(crate) fn is_uhp(path: &str) -> bool {
    path == "/uhp" || path.starts_with("/uhp/")
}

/// The routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard` — flat, never nested, so
/// `api_not_found` keeps seeing the matched path.
pub(crate) fn routes() -> axum::Router<Shared> {
    axum::Router::new()
        .route("/uhp/v1/uhp", get(discovery).fallback(method_not_allowed_fallback))
        .route("/uhp/v1/harnesses", get(harnesses).fallback(method_not_allowed_fallback))
        .route(
            "/uhp/v1/harnesses/{harness_id}",
            get(harness).fallback(method_not_allowed_fallback),
        )
        .route("/uhp/v1/models", get(models).fallback(method_not_allowed_fallback))
        .route("/uhp/v1/sessions", get(sessions).fallback(method_not_allowed_fallback))
        .route(
            "/uhp/v1/sessions/{session_id}",
            get(session).fallback(method_not_allowed_fallback),
        )
        // Version negotiation sits on every served route, before the handler, and stamps
        // `UHP-Version` on whatever comes back. `host_guard` and the unmatched-path fence build
        // their own answers for this surface, with the header on them.
        .route_layer(middleware::from_fn(negotiate_version))
}

/// The 405 a served route answers a method it does not have — every route here is `GET`, so this
/// is what a `POST /uhp/v1/harnesses` reads (the suite's F-02): the envelope, not axum's empty
/// answer, so "an error envelope on every `/uhp` route" holds. axum appends the `Allow` header.
async fn method_not_allowed_fallback() -> Response {
    error(
        StatusCode::METHOD_NOT_ALLOWED,
        "invalid_request_error",
        "method_not_allowed",
        "this UHP route serves GET only",
    )
}

// ---------------------------------------------------------------------------
// Version negotiation and the error envelope
// ---------------------------------------------------------------------------

/// Version negotiation (docs/protocol.md §7.1): a request naming a version this build does not
/// speak is refused with the list of the ones it does, never silently served a different
/// contract. A request naming none gets the default.
async fn negotiate_version(req: Request, next: Next) -> Response {
    let asked = req.headers().get("uhp-version").and_then(|v| v.to_str().ok());
    if let Some(asked) = asked
        && asked != PROTOCOL_VERSION
    {
        return error_with(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "unsupported_protocol_version",
            format!("this server speaks UHP {PROTOCOL_VERSION} only"),
            None,
            Some(json!({ "supported": [PROTOCOL_VERSION] })),
        );
    }
    versioned(next.run(req).await)
}

/// The UHP error envelope (docs/protocol.md §7.7, schema `ErrorEnvelope`): `type` names the class
/// of refusal, `code` the machine-readable cause, `message` one sentence safe to show a user.
fn error(status: StatusCode, kind: &str, code: &str, message: impl Into<String>) -> Response {
    error_with(status, kind, code, message, None, None)
}

/// The envelope with the optional fields filled: `param` names the offending request field,
/// `detail` carries the structured extras a code promises (such as `supported`).
fn error_with(
    status: StatusCode,
    kind: &str,
    code: &str,
    message: impl Into<String>,
    param: Option<&str>,
    detail: Option<Value>,
) -> Response {
    versioned(
        (
            status,
            Json(json!({
                "error": {
                    "type": kind,
                    "code": code,
                    "message": message.into(),
                    "param": param,
                    "detail": detail,
                }
            })),
        )
            .into_response(),
    )
}

/// Stamps `UHP-Version` on a response about to leave the surface.
fn versioned(mut res: Response) -> Response {
    res.headers_mut()
        .insert(VERSION_HEADER, HeaderValue::from_static(PROTOCOL_VERSION));
    res
}

// ---------------------------------------------------------------------------
// The answers `host_guard` and the unmatched-path fence give on this surface
// ---------------------------------------------------------------------------

/// The 401 every `/uhp` path but discovery answers without — or with a wrong — credential: the
/// UHP envelope, not the cockpit's sign-in page, so a protocol client can read the refusal.
pub(crate) fn unauthenticated() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        "authentication_error",
        "invalid_credential",
        "a valid cockpit API token is required for this route",
    )
}

/// A scoped token that authenticated but may not touch the route: the same verdicts `Deny`
/// serves the `/api` surface, in the envelope.
pub(crate) fn denied(deny: api_tokens::Deny) -> Response {
    match deny {
        api_tokens::Deny::Forbidden(message) => error(StatusCode::FORBIDDEN, "permission_error", "insufficient_scope", message),
        api_tokens::Deny::NoSession => error(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "session_not_found",
            "no such session",
        ),
        api_tokens::Deny::NoMap => error(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "not_found",
            "no such UHP route",
        ),
    }
}

/// An unmatched `/uhp` path. `/api` has answered its own misses with a JSON 404 since #641; the
/// SPA fallback must not swallow the protocol's paths the same way it once swallowed those. No
/// response can exist on this surface yet — `POST /uhp/v1/responses` is the follow-up — so a miss
/// under `/responses` names the response, one under `/containers` (§7.5's artifact reads) the
/// file, and anything else the route. The prefixes match on segment boundaries —
/// `/uhp/v1/responsesX` is a route miss, not a response one. The path itself is never echoed
/// back: a probe may carry traversal segments, and the message has no use for them.
pub(crate) fn unmatched(path: &str) -> Response {
    let code = if under(path, "/uhp/v1/responses") {
        "response_not_found"
    } else if under(path, "/uhp/v1/containers") {
        "file_not_found"
    } else {
        "not_found"
    };
    error(StatusCode::NOT_FOUND, "invalid_request_error", code, "no such UHP route")
}

/// Whether `path` is `prefix` itself or a route under it.
fn under(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// `GET /uhp/v1/uhp` — the discovery document, served without credentials (D-02 checks exactly
/// that). The capabilities are the honest list of what this surface serves today: streaming,
/// cancellation and files arrive with the follow-up (docs/protocol.md §7.4–§7.6), and until then
/// the class claim fails the suite's D-05 by design — a false capability reads as "not served",
/// which is true here.
async fn discovery() -> impl IntoResponse {
    Json(json!({
        "object": "uhp.discovery",
        "protocol": "uhp",
        "versions": [PROTOCOL_VERSION],
        "default_version": PROTOCOL_VERSION,
        "conformance_class": "core",
        "capabilities": {
            "sessions": true,
            "session_listing": true,
            "streaming": false,
            "cancellation": false,
            "files_input": false,
            "files_output": false,
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
/// and a switched-off module all read the same `harness_not_found`, the way the colony detail
/// hides what a caller may not see.
async fn harness(State(app): State<Shared>, Path(harness_id): Path<String>) -> Response {
    let found = harness_id
        .strip_prefix("chrn_")
        .filter(|id| !id.is_empty())
        .and_then(|id| app.agents.iter().find(|module| module.id == id));
    let disabled = disabled_pick(&app).await;
    match found {
        Some(module) if disabled.as_deref() != Some(module.id.as_str()) => Json(harness_json(module)).into_response(),
        _ => error(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "harness_not_found",
            "no such harness",
        ),
    }
}

/// The wire shape of one harness (schema `Harness`): `base` is the agent module id a request
/// would name to launch on it, `defaultModel` the manifest's declared default when it declares
/// one. Nothing else the schema allows is fabricated.
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

/// The install's agent-module pick while it is switched off in Settings, if it is: a disabled
/// pick cannot launch a colony, so it reads as absent from this surface. Other installed modules
/// stay listed — an org's `agent.module` override can still launch on them.
async fn disabled_pick(app: &App) -> Option<String> {
    let modules = app.modules.read().await;
    (!modules.agent.enabled).then(|| modules.agent.provider.clone())
}

/// `GET /uhp/v1/models` — the model catalogue. Colonizer's model lists live per provider in the
/// gateway, with availability a function of the stored keys, and no agent module declares a list
/// of its own; the empty catalogue says so rather than inventing entries. Serving the gateway's
/// lists here belongs to the follow-up that serves tasks.
async fn models() -> Json<Value> {
    Json(json!({ "backends": {} }))
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SessionQuery {
    limit: Option<String>,
    cursor: Option<String>,
}

/// `GET /uhp/v1/sessions?limit=&cursor=` — the colony list, filtered and ordered exactly like
/// `/api/sessions` (newest first; a scoped token sees only its orgs and repos) in the UHP page
/// shape. The cursor is the offset into that filtered list; a client treats it as opaque, and one
/// this server did not issue is refused rather than parsed lenient. `limit` is refused only when
/// it is not a whole number; a number outside `1..=MAX_LIMIT` is clamped into it, the way
/// `/api/sessions` caps its page without rejecting the ask.
async fn sessions(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<api_tokens::ScopedToken>>,
    Query(query): Query<SessionQuery>,
) -> Response {
    let limit = match query.limit.as_deref() {
        None => DEFAULT_LIMIT,
        Some(raw) => match raw.parse::<usize>() {
            Ok(parsed) => parsed.clamp(1, MAX_LIMIT),
            Err(_) => return invalid_param("limit", "limit must be a whole number"),
        },
    };
    let offset = match query.cursor.as_deref() {
        None => 0,
        Some(raw) => match raw.parse::<usize>() {
            Ok(parsed) => parsed,
            Err(_) => return invalid_param("cursor", "cursor must be one this server issued"),
        },
    };
    let all = app.sessions.read().await.clone();
    let visible: Vec<&Session> = all
        .iter()
        .rev()
        // A scoped token's org/repo limits are also this list's filter, as on `/api/sessions`.
        .filter(|session| match &scoped {
            Some(axum::Extension(token)) => token.covers(&session.org, &session.repo),
            None => true,
        })
        .collect();
    let next = offset + limit;
    let next_cursor = (visible.len() > next).then(|| next.to_string());
    let page: Vec<Value> = visible
        .iter()
        .skip(offset)
        .take(limit)
        .map(|session| session_json(session))
        .collect();
    versioned(
        Json(json!({
            "object": "list",
            "sessions": page,
            "next_cursor": next_cursor,
        }))
        .into_response(),
    )
}

/// `GET /uhp/v1/sessions/{session_id}` — one colony. An id outside a scoped token's limits reads
/// exactly like an unknown one: `authorize` has already turned the 403 into a 404 for `/api`,
/// and this handler repeats the hiding in case a future caller reaches it another way.
async fn session(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<api_tokens::ScopedToken>>,
    Path(session_id): Path<String>,
) -> Response {
    let found = app.session(&session_id).await;
    let visible = match (&found, &scoped) {
        (Some(session), Some(axum::Extension(token))) => token.covers(&session.org, &session.repo),
        (Some(_), None) => true,
        (None, _) => false,
    };
    match found.filter(|_| visible) {
        Some(session) => Json(session_json(&session)).into_response(),
        None => error(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "session_not_found",
            "no such session",
        ),
    }
}

/// A refused query parameter: 400 `invalid_input` with `param` naming the field (§7.7).
fn invalid_param(param: &str, message: &str) -> Response {
    error_with(
        StatusCode::BAD_REQUEST,
        "invalid_request_error",
        "invalid_input",
        message,
        Some(param),
        None,
    )
}

/// The UHP session (schema `Session`): the colony's own status word, the module it launched on as
/// `harness_id`, and the creation time in unix seconds. The rest of `Session` stays on
/// `/api/sessions`, which serves it whole.
fn session_json(session: &Session) -> Value {
    json!({
        "object": "session",
        "id": session.id,
        "harness_id": format!("chrn_{}", session.agent),
        "status": session.status.as_str(),
        "created_at": session.created_at.timestamp(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_tokens::NewToken;
    use crate::server::router;
    use crate::sessions::tests::colony;
    use crate::tests::{temp_root, test_app, test_app_with_agents};
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::{Method, header};
    use serde_json::Value;
    use tower::ServiceExt as _;

    /// The discovery literal and the path `host_guard` exempts must stay the same string: the
    /// route table reads the literal, the guard reads the constant.
    #[test]
    fn the_discovery_route_constant_names_the_served_route() {
        assert_eq!(DISCOVERY_PATH, "/uhp/v1/uhp");
    }

    fn get(uri: &str, headers: Vec<(HeaderName, String)>) -> Request {
        let mut builder = Request::builder().method(Method::GET).uri(uri);
        builder = builder.header(header::HOST, "127.0.0.1:7878");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn bearer(app: &Shared) -> (HeaderName, String) {
        (header::AUTHORIZATION, format!("Bearer {}", app.api_token))
    }

    async fn body_json(res: Response) -> Value {
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn error_of(body: &Value) -> serde_json::Map<String, Value> {
        body["error"].as_object().unwrap().clone()
    }

    /// A module manifest the way `discover_agents` builds one, with a `model` setting default
    /// when the manifest declares one.
    fn agent(id: &str, name: &str, model_default: Option<&str>) -> AgentModule {
        let mut schema = json!({"type": "object", "properties": {}});
        if let Some(default) = model_default {
            schema["properties"]["model"] = json!({"type": "string", "default": default});
        }
        AgentModule {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            dir: std::path::PathBuf::from(format!("/assets/modules/agents/{id}")),
            entry: vec!["run".into()],
            needs_claude: false,
            schema,
            requires: crate::modules::Requires::default(),
            egress: None,
            resume_dir: None,
        }
    }

    #[tokio::test]
    async fn discovery_answers_without_credentials_and_states_the_honest_surface() {
        let root = temp_root();
        let app = test_app(&root);
        let res = router(&app).oneshot(get(DISCOVERY_PATH, vec![])).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get("uhp-version").map(|v| v.as_bytes()),
            Some(PROTOCOL_VERSION.as_bytes()),
            "every /uhp answer names its version"
        );
        let body = body_json(res).await;
        assert_eq!(body["object"], "uhp.discovery");
        assert_eq!(body["protocol"], "uhp");
        assert_eq!(body["versions"], json!([PROTOCOL_VERSION]));
        assert_eq!(body["default_version"], PROTOCOL_VERSION);
        assert_eq!(body["conformance_class"], "core");
        assert_eq!(body["implementation"]["name"], "colonizer");
        // The capabilities say what is served, not what is planned: the task-bearing halves of
        // the core class are the follow-up, and a client must be able to trust the false ones.
        for (cap, want) in [
            ("sessions", true),
            ("session_listing", true),
            ("streaming", false),
            ("cancellation", false),
            ("files_input", false),
            ("files_output", false),
            ("harness_management", false),
            ("session_sharing", false),
            ("plugins", false),
        ] {
            assert_eq!(body["capabilities"][cap], want, "capability {cap}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unsupported_version_is_refused_with_the_supported_one() {
        let root = temp_root();
        let app = test_app(&root);
        let res = router(&app)
            .oneshot(get(
                DISCOVERY_PATH,
                vec![("UHP-Version".parse().unwrap(), "1999-01-01".into())],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            res.headers().get("uhp-version").unwrap(),
            PROTOCOL_VERSION,
            "even the refusal names the version it speaks"
        );
        let err = error_of(&body_json(res).await);
        assert_eq!(err["type"], "invalid_request_error");
        assert_eq!(err["code"], "unsupported_protocol_version");
        assert_eq!(err["detail"]["supported"], json!([PROTOCOL_VERSION]));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn harness_routes_sit_behind_the_token_wall_in_uhp_envelopes() {
        let root = temp_root();
        let app = test_app(&root);
        for headers in [vec![], vec![(header::AUTHORIZATION, "Bearer not-a-token".into())]] {
            let res = router(&app).oneshot(get("/uhp/v1/harnesses", headers)).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(res.headers().get("uhp-version").unwrap(), PROTOCOL_VERSION);
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
        let body = body_json(res).await;
        assert_eq!(body["object"], "list");
        let listed = body["harnesses"].as_array().unwrap();
        assert_eq!(listed.len(), 2, "{body}");
        assert_eq!(listed[0]["id"], "chrn_claude-code");
        assert_eq!(listed[0]["base"], "claude-code");
        assert_eq!(listed[0]["object"], "harness");
        assert!(
            listed[0].get("defaultModel").is_none(),
            "no default is invented where the manifest declares none"
        );
        assert_eq!(listed[1]["defaultModel"], "xai-grok/grok-4.5");

        // Detail: the declared one, then the refusals — unknown and prefixless read the same.
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
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_switched_off_pick_is_no_harness_at_all() {
        let root = temp_root();
        let app = test_app_with_agents(&root, vec![agent("claude-code", "Claude Code", None)], |cfg| {
            cfg.allowed_hosts = Vec::new();
        });
        {
            let mut modules = app.modules.write().await;
            modules.agent.enabled = false;
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

    /// The protocol paths that no module claims yet are 404 envelopes, not pages: a missing
    /// response names the response, and a traversal probe against the not-yet-served artifact
    /// route is refused without echoing anything it carried. The prefix reads on segment
    /// boundaries — `/uhp/v1/responsesX` is a route miss, not a response one.
    #[tokio::test]
    async fn unmatched_uhp_paths_answer_envelopes_and_never_serve_a_page() {
        let root = temp_root();
        let app = test_app(&root);
        let router = router(&app);
        let res = router
            .clone()
            .oneshot(get("/uhp/v1/responses/resp_nosuch", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let err = error_of(&body_json(res).await);
        assert_eq!(err["code"], "response_not_found");

        for (probe, code) in [("/uhp/v1/responsesX", "not_found"), ("/uhp/v1/containers", "file_not_found")] {
            let res = router.clone().oneshot(get(probe, vec![bearer(&app)])).await.unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{probe}");
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
            let text = {
                let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
                String::from_utf8(bytes.to_vec()).unwrap()
            };
            assert!(!text.contains("root:"), "{probe} served a file: {text}");
            assert!(!text.contains("etc/passwd"), "{probe} echoed the path");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// A method a served route does not have is the envelope too — the suite's F-02 reads exactly
    /// this on `POST /uhp/v1/harnesses` — with `Allow` naming what is served and `UHP-Version` on
    /// the answer, so no `/uhp` route breaks the envelope contract.
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
        ] {
            let req = Request::builder()
                .method(method.clone())
                .uri(uri)
                .header(header::HOST, "127.0.0.1:7878")
                .header(header::AUTHORIZATION, format!("Bearer {}", app.api_token))
                .body(Body::empty())
                .unwrap();
            let res = router.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
            assert_eq!(
                res.headers().get("uhp-version").map(|v| v.as_bytes()),
                Some(PROTOCOL_VERSION.as_bytes()),
                "{method} {uri}"
            );
            assert_eq!(res.headers().get(header::ALLOW).unwrap(), "GET,HEAD", "{method} {uri}");
            let err = error_of(&body_json(res).await);
            assert_eq!(err["type"], "invalid_request_error", "{method} {uri}");
            assert_eq!(err["code"], "method_not_allowed", "{method} {uri}");
            assert!(err["message"].is_string());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn sessions_page_by_limit_and_cursor_like_the_api_list() {
        let root = temp_root();
        let app = test_app(&root);
        for (id, status) in [
            ("old", crate::sessions::SessionStatus::Stopped),
            ("mid", crate::sessions::SessionStatus::Running),
            ("new", crate::sessions::SessionStatus::Queued),
        ] {
            let mut session = colony("acme", status);
            session.id = id.into();
            session.agent = "claude-code".into();
            app.sessions.write().await.push(session);
        }
        let router = router(&app);
        let headers = vec![bearer(&app)];

        let res = router
            .clone()
            .oneshot(get("/uhp/v1/sessions", headers.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["object"], "list");
        assert_eq!(body["next_cursor"], Value::Null, "three of a default page, no more");
        let listed = body["sessions"].as_array().unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0]["id"], "new", "newest first, like /api/sessions");
        assert_eq!(listed[0]["object"], "session");
        assert_eq!(listed[0]["harness_id"], "chrn_claude-code");
        assert_eq!(listed[0]["status"], "queued");
        assert!(listed[0]["created_at"].is_number(), "session shape: {body}");

        // A short page says so in `next_cursor`, and the cursor walks the rest.
        let res = router
            .clone()
            .oneshot(get("/uhp/v1/sessions?limit=1", headers.clone()))
            .await
            .unwrap();
        let body = body_json(res).await;
        assert_eq!(body["sessions"].as_array().unwrap().len(), 1);
        let cursor = body["next_cursor"].as_str().unwrap().to_string();
        let res = router
            .clone()
            .oneshot(get(&format!("/uhp/v1/sessions?limit=2&cursor={cursor}"), headers))
            .await
            .unwrap();
        let body = body_json(res).await;
        let rest = body["sessions"].as_array().unwrap();
        assert_eq!(rest.len(), 2, "{body}");
        assert_eq!(rest[0]["id"], "mid");
        assert_eq!(body["next_cursor"], Value::Null, "the walk is over");

        // A limit or cursor the server did not issue is a 400 naming the parameter.
        for (query, param) in [("limit=lots", "limit"), ("cursor=nope", "cursor")] {
            let res = router
                .clone()
                .oneshot(get(&format!("/uhp/v1/sessions?{query}"), vec![bearer(&app)]))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{query}");
            let err = error_of(&body_json(res).await);
            assert_eq!(err["code"], "invalid_input", "{query}");
            assert_eq!(err["param"], param, "{query}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #650: a scoped `read` token watches through UHP exactly as through `/api` — the
    /// list is filtered to its orgs, and a colony outside them reads as unknown, never as 403.
    #[tokio::test]
    async fn a_scoped_read_token_is_held_to_its_limits() {
        let root = temp_root();
        let app = test_app(&root);
        let made = app
            .api_tokens
            .create(NewToken {
                name: "ci".into(),
                scope: "read".into(),
                orgs: vec!["acme".into()],
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            })
            .await
            .unwrap();
        for (id, org) in [("mine", "acme"), ("theirs", "other")] {
            let mut session = colony(org, crate::sessions::SessionStatus::Running);
            session.id = id.into();
            app.sessions.write().await.push(session);
        }
        let router = router(&app);
        let scoped_bearer = vec![(header::AUTHORIZATION, format!("Bearer {}", made.token))];

        let res = router
            .clone()
            .oneshot(get("/uhp/v1/sessions", scoped_bearer.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        let listed = body["sessions"].as_array().unwrap();
        assert_eq!(listed.len(), 1, "{body}");
        assert_eq!(listed[0]["id"], "mine");

        let res = router
            .clone()
            .oneshot(get("/uhp/v1/sessions/theirs", scoped_bearer.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(error_of(&body_json(res).await)["code"], "session_not_found");
        let res = router
            .clone()
            .oneshot(get("/uhp/v1/sessions/mine", scoped_bearer.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["id"], "mine");

        // `read` covers every served route here, so the permission error needs a path the
        // allowlist does not list: a scoped token may not probe the surface's corners either.
        let res = router.oneshot(get("/uhp/v1/does-not-exist", scoped_bearer)).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let err = error_of(&body_json(res).await);
        assert_eq!(err["type"], "permission_error");
        assert_eq!(err["code"], "insufficient_scope");
        let _ = std::fs::remove_dir_all(root);
    }
}
