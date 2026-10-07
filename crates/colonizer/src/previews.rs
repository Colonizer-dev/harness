//! Dev-server previews (issue #690): the owner points a colony at a guest-local port and browsers
//! reach it through `/api/previews/{id}/…`, a small reverse proxy over the private mesh. Every
//! credential the caller presented is stripped before the request is forwarded, and the preview
//! closes with the colony. Previews need the mesh; WebSocket/HMR upgrades are out of scope.

use crate::{
    ApiResult, App, Shared,
    api_tokens::{Scope, ScopedToken},
    client_error,
    sessions::{AGENTD_PORT, Io, Session, find_headers_end},
};
use anyhow::Context;
use axum::{
    Json,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One proxy round trip is a local dial plus a dev server's answer: seconds, not minutes.
const TIMEOUT: Duration = Duration::from_secs(30);
/// The most of a request body forwarded, and of a response buffered before giving up.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// The body of `POST /api/sessions/{id}/preview`.
#[derive(Deserialize)]
pub(crate) struct SetPreview {
    port: u16,
}

/// `POST /api/sessions/{id}/preview`: open a preview on a guest-local port. Owner-only — the route
/// is absent from `api_tokens::classify`, so a scoped token never reaches it.
pub(crate) async fn set_preview(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(body): Json<SetPreview>,
) -> ApiResult<Value> {
    if body.port == 0 || body.port == AGENTD_PORT {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("port must be 1-65535 and must not be {AGENTD_PORT}, the colony's own agentd port"),
        ));
    }
    let Some(s) = app.session(&id).await else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    if !s.status.is_live() || s.suspended.is_some() {
        return Err(client_error(StatusCode::CONFLICT, "the colony is not running with a microVM"));
    }
    app.update_session(&id, |x| x.preview_port = Some(body.port)).await;
    Ok(Json(json!({"url": format!("/api/previews/{id}/")})))
}

/// `DELETE /api/sessions/{id}/preview`: close it.
pub(crate) async fn clear_preview(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    if app.session(&id).await.is_none() {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    }
    app.update_session(&id, |x| x.preview_port = None).await;
    Ok(Json(json!({"ok": true})))
}

/// `/api/previews/{id}/…`: the reverse proxy. The owner and a fleet token both reach it (the route
/// is `Need::Fleet`); a fleet caller must pass the fleet network policy, and every credential and
/// hop-by-hop header is stripped before the request goes on to the colony.
pub(crate) async fn proxy(State(app): State<Shared>, req: Request) -> Response {
    let Some((id, guest_path)) = preview_target(req.uri().path()) else {
        return not_found();
    };
    let Some(s) = app.session(&id).await else {
        return not_found();
    };
    // Closed as soon as the colony is not live: the port lives only as long as the microVM.
    let Some(port) = s.preview_port.filter(|_| s.status.is_live() && s.suspended.is_none()) else {
        return not_found();
    };
    // A fleet token authenticates only on the owner, so a fleet caller is always reaching an owner
    // colony: the policy's `from` is the member, `to` the owner.
    if let Some(token) = req.extensions().get::<ScopedToken>().cloned()
        && token.scope == Scope::Fleet
        && app
            .fleet_members
            .member_for_token(&token.id)
            .await
            .is_none_or(|m| !fleet_may_reach(&app, &m))
    {
        return fail(StatusCode::FORBIDDEN, "the fleet network policy bars this machine");
    }
    let Some(ip) = s.mesh.as_ref().and_then(|m| m.ip.clone()) else {
        return fail(StatusCode::CONFLICT, "previews need the mesh, which is off on this host");
    };
    let method = req.method().clone();
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let headers = req.headers().clone();
    let body = match axum::body::to_bytes(req.into_body(), MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return fail(StatusCode::PAYLOAD_TOO_LARGE, "the preview request body is too large"),
    };
    let host = format!("{ip}:{port}");
    let head = forwarding_head(&method, &format!("{guest_path}{query}"), &host, &headers, body.len());
    match tokio::time::timeout(TIMEOUT, forward(&app, &s, port, head.into_bytes(), body.to_vec())).await {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => client_error(
            StatusCode::BAD_GATEWAY,
            &format!("the colony's preview did not answer: {e:#}"),
        )
        .into_response(),
        Err(_) => fail(StatusCode::GATEWAY_TIMEOUT, "the colony's preview did not answer in time"),
    }
}

/// The API routes this module serves. The preview's methods are listed rather than `any`, so the
/// route table's `OPTIONS` probe still reads them as a method fallback (`routes/previews.snap`).
pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing::{get, post};
    let preview = get(proxy).post(proxy).put(proxy).patch(proxy).delete(proxy);
    axum::Router::new()
        .route("/api/sessions/{id}/preview", post(set_preview).delete(clear_preview))
        .route("/api/previews/{id}/", preview.clone())
        .route("/api/previews/{id}/{*rest}", preview)
}

/// Splits `/api/previews/{id}/rest` into the colony id and the guest path (`/rest`, or `/` for the
/// bare prefix). `None` for any other path, or one naming no colony.
fn preview_target(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix("/api/previews/")?;
    let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
    (!id.is_empty()).then(|| (id.to_string(), format!("/{tail}")))
}

/// The HTTP/1.0 request head sent to the guest: the method and path as received, the guest as
/// `Host`, every header but the credentials and hop-by-hop ones, then the body's length and
/// `Connection: close`. HTTP/1.0 stops the guest answering chunked, so one bounded read frames it.
fn forwarding_head(method: &Method, target: &str, host: &str, headers: &HeaderMap, body_len: usize) -> String {
    let mut head = format!("{method} {target} HTTP/1.0\r\nHost: {host}\r\n");
    for (name, value) in headers {
        if let Some(value) = value.to_str().ok().filter(|_| is_forwarded_header(name)) {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    head.push_str(&format!("Content-Length: {body_len}\r\nConnection: close\r\n\r\n"));
    head
}

/// Headers the proxy never passes on, in either direction: every credential the caller presented
/// (the owner and fleet tokens, the cookie, any `x-colonizer-*` header) and the hop-by-hop ones.
fn is_forwarded_header(name: &HeaderName) -> bool {
    let name = name.as_str();
    !matches!(
        name,
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "upgrade"
            | "keep-alive"
            | "te"
            | "trailer"
    ) && !name.starts_with("x-colonizer")
}

/// Dials the preview port on the colony's microVM over the mesh. A mesh-off build never gets here
/// (the handler answers `409`); the tests point one session's dial at a local listener.
async fn dial_guest(app: &App, s: &Session, port: u16) -> anyhow::Result<Box<dyn Io>> {
    // Keyed by session id so parallel tests cannot interleave, and read into a local before the
    // `connect` await — a `MutexGuard` held across it would make the handler's future non-`Send`.
    #[cfg(test)]
    if let Some(addr) = tests::override_for(&s.id) {
        return Ok(Box::new(tokio::net::TcpStream::connect(addr).await?));
    }
    let ip = s
        .mesh
        .as_ref()
        .and_then(|m| m.ip.clone())
        .context("the microVM's address is not known yet")?;
    Ok(Box::new(app.mesh().await?.dial(&ip, port).await?))
}

/// Sends one request head and body to the guest and reads its answer back as a `Response`.
async fn forward(app: &App, s: &Session, port: u16, head: Vec<u8>, body: Vec<u8>) -> anyhow::Result<Response> {
    let mut stream = dial_guest(app, s, port).await?;
    stream.write_all(&head).await?;
    if !body.is_empty() {
        stream.write_all(&body).await?;
    }
    stream.flush().await?;
    let (status, headers, body) = read_response(&mut stream).await?;
    let len = body.len();
    let mut response = Response::new(axum::body::Body::from(body));
    *response.status_mut() = StatusCode::from_u16(status).context("the preview sent an invalid status")?;
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            value.parse::<axum::http::HeaderValue>(),
        ) && is_forwarded_header(&name)
        {
            // `append`, not `insert`: a dev server may set several `Set-Cookie`s.
            response.headers_mut().append(name, value);
        }
    }
    // Our own framing: the body is buffered, so the length is known.
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, axum::http::HeaderValue::from(len));
    Ok(response)
}

/// Reads the whole HTTP/1.0 response in one capped read: the guest answered without chunking and
/// closes at EOF (the request said `Connection: close`), so headers and body arrive together, all
/// of it bounded by [`MAX_RESPONSE_BYTES`].
async fn read_response(stream: &mut Box<dyn Io>) -> anyhow::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut raw = Vec::new();
    stream.take(MAX_RESPONSE_BYTES as u64 + 1).read_to_end(&mut raw).await?;
    if raw.len() > MAX_RESPONSE_BYTES {
        anyhow::bail!("the preview's response is larger than {MAX_RESPONSE_BYTES} bytes");
    }
    let head_end = find_headers_end(&raw).context("the preview response has no header terminator")?;
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .context("malformed preview response")?;
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| {
            line.split_once(':')
                .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    Ok((status, headers, raw[head_end..].to_vec()))
}

/// Whether the fleet network policy lets member `from_member` reach this host. A fleet token
/// authenticates only on the owner, so `to` is always the owner; no policy means no limit.
fn fleet_may_reach(app: &App, from_member: &str) -> bool {
    crate::fleet_policy::load(&app.cfg.config_dir).is_none_or(|policy| policy.may_reach(from_member, "owner"))
}

/// A response in the API's error shape.
fn fail(status: StatusCode, message: &str) -> Response {
    client_error(status, message).into_response()
}

/// The 404 every closed or unknown preview answers.
fn not_found() -> Response {
    fail(StatusCode::NOT_FOUND, "no such preview")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sessions::{MeshInfo, SessionStatus, tests::app_with_colony};
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt as _;

    /// Where [`dial_guest`] connects in tests, keyed by session id: a local dev server stands in for
    /// the guest's port, and parallel tests cannot interleave.
    static DIAL_OVERRIDE: Mutex<Vec<(String, SocketAddr)>> = Mutex::new(Vec::new());

    pub(super) fn override_for(id: &str) -> Option<SocketAddr> {
        let guard = DIAL_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner());
        guard.iter().find(|(sid, _)| sid == id).map(|(_, addr)| *addr)
    }

    /// A one-shot HTTP/1.0 dev server: it captures the request head (proving no credential rode
    /// along) and answers `body`, then closes.
    async fn dev_server(body: &'static str) -> (SocketAddr, Arc<Mutex<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(String::new()));
        let capture = seen.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") && stream.read_buf(&mut head).await.unwrap() != 0 {}
            *capture.lock().unwrap() = String::from_utf8_lossy(&head).into_owned();
            let response = format!(
                "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        });
        (addr, seen)
    }

    /// The module's routes behind the real `host_guard`, as the mothership serves them.
    fn guarded(app: &Shared) -> axum::Router<()> {
        crate::previews::routes()
            .layer(axum::middleware::from_fn_with_state(app.clone(), crate::server::host_guard))
            .with_state(app.clone())
    }

    fn request(method: Method, uri: &str, headers: Vec<(HeaderName, String)>) -> Request {
        let mut builder = Request::builder().method(method).uri(uri).header(header::HOST, "127.0.0.1");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        builder.body(axum::body::Body::from("")).unwrap()
    }

    async fn send(app: &Shared, req: Request) -> Response {
        guarded(app).oneshot(req).await.unwrap()
    }

    async fn body_text(res: Response) -> String {
        String::from_utf8(axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap()
    }

    /// A member's fleet token, minted as pairing would.
    async fn fleet_token(app: &Shared) -> String {
        crate::fleet_members::FleetStore::add_member_for_tests(app, "laptop").await.1
    }

    /// A running colony with a mesh address and an open preview on 5173.
    async fn colony_with_preview() -> (Shared, std::path::PathBuf) {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        app.update_session("abc", |s| {
            s.mesh = Some(MeshInfo {
                name: "colonizer-abc".into(),
                ip: Some("10.0.0.9".into()),
            });
            s.preview_port = Some(5173);
        })
        .await;
        (app, root)
    }

    /// No token: the guard answers 401 before the proxy runs.
    #[tokio::test]
    async fn an_unauthenticated_preview_request_is_rejected() {
        let (app, root) = colony_with_preview().await;
        let res = send(&app, request(Method::GET, "/api/previews/abc/", vec![])).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A paired member's fleet token reaches an open preview; the dev server sees no Authorization
    /// header, and its body crosses over intact.
    #[tokio::test]
    async fn a_fleet_token_reaches_the_preview_and_no_credential_is_forwarded() {
        let (app, root) = colony_with_preview().await;
        let (addr, seen) = dev_server("hello from vite").await;
        DIAL_OVERRIDE.lock().unwrap().push(("abc".into(), addr));
        let token = fleet_token(&app).await;

        let headers = vec![(header::AUTHORIZATION, format!("Bearer {token}"))];
        let res = send(&app, request(Method::GET, "/api/previews/abc/", headers)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_text(res).await, "hello from vite");

        let head = seen.lock().unwrap().clone();
        assert!(head.starts_with("GET / HTTP/1.0"), "{head}");
        assert!(head.contains("Host: 10.0.0.9:5173"), "Host is the guest: {head}");
        assert!(
            !head.to_ascii_lowercase().contains("authorization") && !head.to_ascii_lowercase().contains("cookie"),
            "the fleet token must not reach the colony: {head}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Once the colony stops, the preview is closed: a 404, never a dial.
    #[tokio::test]
    async fn a_stopped_colonys_preview_is_gone() {
        let (app, root) = colony_with_preview().await;
        app.update_session("abc", |s| s.status = SessionStatus::Stopped).await;
        let headers = vec![(header::AUTHORIZATION, format!("Bearer {}", app.api_token))];
        let res = send(&app, request(Method::GET, "/api/previews/abc/", headers)).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Opening and closing a preview is the owner's: a fleet token is refused.
    #[tokio::test]
    async fn a_fleet_token_cannot_open_a_preview() {
        let (app, root) = colony_with_preview().await;
        let token = fleet_token(&app).await;
        let headers = vec![
            (header::AUTHORIZATION, format!("Bearer {token}")),
            (header::CONTENT_TYPE, "application/json".into()),
        ];
        let mut req = request(Method::POST, "/api/sessions/abc/preview", headers);
        *req.body_mut() = axum::body::Body::from(r#"{"port": 5173}"#);
        assert_eq!(send(&app, req).await.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
    }
}
