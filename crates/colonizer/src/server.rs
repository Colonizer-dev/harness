//! The mothership as a server: `serve` loads the state, assembles the router from every module's
//! `routes()`, starts every module's background work and serves until a signal.
//!
//! Adding a module: give it `pub(crate) fn routes() -> Router<Shared>` with its own routes and
//! layers and, if it runs in the background, `pub(crate) fn start_tasks(app: &Shared)`. Then add
//! one line to the alphabetical list in `api_routes` and one to the list in `start_tasks`, and
//! regenerate `routes.snap`. CONTRIBUTING.md has the whole recipe.

use crate::app::{Boot, load_sessions};
use crate::config::{ModulesConfig, Settings};
use crate::{
    App, Shared, StorageAlert, activity, api_tokens, auth, gateway, login_item, modules, remote, secrets, sessions, usage,
};
use anyhow::{Context, Result, bail};
use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
};
use std::{path::Path as FsPath, sync::Arc};
use tower_http::services::{ServeDir, ServeFile};

const UI_MISSING_HTML: &str = "<!doctype html><title>Colonizer</title>\
<body style=\"font:15px system-ui;margin:3rem\"><h1>Colonizer is running</h1>\
<p>The web UI isn't built yet. Run <code>scripts/install.sh</code> (or <code>npm run build</code> in <code>web/</code>).</p>";

/// Rejects DNS rebinding (unexpected Host), then requires the per-install API token (issue #405):
/// `Authorization: Bearer` or the `colonizer_token` cookie. Bearer requests skip the `Origin`
/// check (no CORS preflight is ever granted); cookie writes and upgrades keep the same-origin
/// requirement. Unauthenticated `GET /api/status` answers the reduced body; other `/api` requests
/// get a 401; page loads get the sign-in page, or set the cookie from the link's `?token=`.
/// A request that arrived through the remote tunnel (remote.rs, issue #533) skips the allowlist:
/// it is admitted only while remote access is on, only under its own tunnel host, and the Origin
/// fence accepts exactly `https://<host>` for it — the tunnel host is never a LAN host.
pub(crate) async fn host_guard(State(app): State<Shared>, mut req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let tunnelled = match req.extensions().get::<remote::Tunnelled>().map(|t| t.host.as_str()) {
        Some(tunnel_host) if tunnel_host == host && app.remote.enabled().await => true,
        Some(_) => return (StatusCode::SERVICE_UNAVAILABLE, "remote access is off").into_response(),
        None => false,
    };
    let hostname = if host.starts_with('[') {
        host.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or_default().to_string()
    };
    let bind_host = app.cfg.bind.rsplit_once(':').map_or(app.cfg.bind.as_str(), |(h, _)| h);
    let allowed = tunnelled
        || matches!(hostname.as_str(), "localhost" | "127.0.0.1" | "[::1]")
        || hostname == bind_host
        || app.cfg.allowed_hosts.contains(&hostname);
    if !allowed {
        return (StatusCode::FORBIDDEN, "Host not allowed (set COLONIZER_ALLOWED_HOSTS)").into_response();
    }
    // Writes and upgrades need a same-origin `Origin`, or a request no browser could have made:
    // a missing `Origin` is rejected rather than trusted (see #375), and only an authenticated
    // request passes at all.
    let upgrade = req.headers().contains_key(header::UPGRADE);
    let bearer_ok = auth::bearer_token(req.headers()).is_some_and(|token| auth::tokens_match(&token, &app.api_token));
    let cookie_ok = auth::cookie_token(req.headers()).is_some_and(|token| auth::tokens_match(&token, &app.api_token));
    if bearer_ok || cookie_ok {
        // Cookie-authenticated writes and upgrades keep the same-origin requirement; header
        // authentication already proves a non-browser caller.
        if !bearer_ok && (req.method() != Method::GET || upgrade) {
            let same_origin = if tunnelled {
                // Through the tunnel the cockpit is served from https://<host>, and nothing else.
                req.headers().get(header::ORIGIN).and_then(|o| o.to_str().ok()) == Some(format!("https://{host}").as_str())
            } else {
                req.headers()
                    .get(header::ORIGIN)
                    .and_then(|o| o.to_str().ok())
                    .is_some_and(|origin| origin.split("://").nth(1) == Some(host.as_str()))
            };
            if !same_origin {
                return (StatusCode::FORBIDDEN, "cross-origin request rejected").into_response();
            }
        }
        req.extensions_mut().insert(auth::Authenticated(true));
        // Who the activity log says acted: the browser (cookie) or a token holder (the CLI, a script).
        req.extensions_mut()
            .insert(if bearer_ok { auth::Via::Api } else { auth::Via::Cockpit });
        return next.run(req).await;
    }
    // A scoped API token (`col_…`, issue #508) authenticates by Bearer header only — a browser
    // never holds one, so the cookie stays owner-only. `authorize` decides the route from the
    // token's scope and org/repo limits (403 off the allowlist, 404 outside its colonies); what
    // it allows, the request carries onward as authenticated, with the token attached for the
    // handlers that must know who is acting (launch checks, the list filter, the prompt marking).
    if let Some(token) = auth::bearer_token(req.headers())
        && let Some(scoped) = app.api_tokens.authenticate(&token).await
    {
        if let Err(deny) = api_tokens::authorize(&app, &scoped, req.method(), req.uri().path()).await {
            return deny.into_response();
        }
        req.extensions_mut().insert(auth::Authenticated(true));
        req.extensions_mut().insert(auth::Via::Token(scoped.name.clone()));
        req.extensions_mut().insert(scoped);
        return next.run(req).await;
    }
    // No valid token: the reduced status, the sign-in link's cookie, or how to sign in.
    let path = req.uri().path().to_string();
    if path == "/api" || path.starts_with("/api/") {
        if req.method() == Method::GET && path == "/api/status" {
            req.extensions_mut().insert(auth::Authenticated(false));
            return next.run(req).await;
        }
        return (StatusCode::UNAUTHORIZED, auth::UNAUTHORIZED_BODY).into_response();
    }
    // The installable-app files carry no secrets, and browsers fetch the manifest without the
    // cookie: they load before sign-in, so the locked page still installs and shows its icon.
    if req.method() == Method::GET && is_public_app_file(&path) {
        req.extensions_mut().insert(auth::Authenticated(false));
        return next.run(req).await;
    }
    if req.method() == Method::GET
        && let Some(token) = auth::query_token(req.uri().query())
        && auth::tokens_match(&token, &app.api_token)
    {
        // The token must not linger in caches or leak via the Referer header on the next click.
        let mut res = Html(auth::login_page()).into_response();
        let headers = res.headers_mut();
        if let Ok(cookie) = auth::set_cookie_header(&app.api_token).parse() {
            headers.insert(header::SET_COOKIE, cookie);
        }
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
        return res;
    }
    let mut res = (StatusCode::UNAUTHORIZED, Html(auth::locked_page())).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// The PWA files served before sign-in: the manifest, the service worker, its routing script, the
/// offline page and the icons. Nothing under `/assets` or `/api`.
fn is_public_app_file(path: &str) -> bool {
    matches!(path, "/manifest.webmanifest" | "/sw.js" | "/sw-routes.js" | "/offline.html")
        || (path.starts_with("/icons/") && !path.contains("..") && path.len() < 64)
}

async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

fn web_router(assets: Option<&FsPath>) -> Router<Shared> {
    match assets.map(|a| a.join("web")).filter(|dir| dir.join("index.html").exists()) {
        // Hashed build assets 404 when missing instead of falling back to the page: a tab left open
        // across an update asks for chunks the new build no longer has, and HTML served as a
        // module script fails with a MIME error the page cannot tell apart from a real bug.
        Some(dir) => Router::new()
            .nest_service("/assets", ServeDir::new(dir.join("assets")))
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(dir.join("index.html")))),
        None => Router::new().fallback(|| async { Html(UI_MISSING_HTML) }),
    }
}

/// Every API route, before any layer: `router` wraps them in the activity log's route layer and
/// `host_guard`. Each module keeps its own routes in its `routes()`.
pub(crate) fn api_routes() -> Router<Shared> {
    // One line per module that serves API routes, kept in alphabetical order.
    Router::new()
        .merge(crate::activity::routes())
        .merge(crate::api_tokens::routes())
        .merge(crate::archive::routes())
        .merge(crate::burn_down::routes())
        .merge(crate::chat::routes())
        .merge(crate::chat_images::routes())
        .merge(crate::claude_accounts::routes())
        .merge(crate::claude_login::routes())
        .merge(crate::code::routes())
        .merge(crate::colonize::routes())
        .merge(crate::colony_secrets::routes())
        .merge(crate::deps::routes())
        .merge(crate::egress::routes())
        .merge(crate::findings::routes())
        .merge(crate::fleet::routes())
        .merge(crate::gateway::routes())
        .merge(crate::github::routes())
        .merge(crate::graft::routes())
        .merge(crate::headroom::routes())
        .merge(crate::hunters::routes())
        .merge(crate::img_proxy::routes())
        .merge(crate::lifecycle::routes())
        .merge(crate::login_item::routes())
        .merge(crate::loops::routes())
        .merge(crate::maps::routes())
        .merge(crate::memory::routes())
        .merge(crate::modules::routes())
        .merge(crate::notify::routes())
        .merge(crate::orgs::routes())
        .merge(crate::packages::routes())
        .merge(crate::plugins::routes())
        .merge(crate::providers::routes())
        .merge(crate::publish::routes())
        .merge(crate::push::routes())
        .merge(crate::reclaim::routes())
        .merge(crate::redteam::routes())
        .merge(crate::remote::routes())
        .merge(crate::repo_meta::routes())
        .merge(crate::sandbox::routes())
        .merge(crate::secrets::routes())
        .merge(crate::sessions::routes())
        .merge(crate::spend::routes())
        .merge(crate::stale::routes())
        .merge(crate::status::routes())
        .merge(crate::stream::routes())
        .merge(crate::telemetry::routes())
        .merge(crate::update::routes())
        .merge(crate::usage::routes())
        .merge(crate::version::routes())
        .merge(crate::voice::routes())
}

/// The whole cockpit: the API behind the activity log's route layer, the web UI, and `host_guard`
/// in front of both.
pub(crate) fn router(app: &Shared) -> Router {
    api_routes()
        // After every route: records what a person changed through the API (activity.rs). A
        // route layer, so it sees the matched route, and inside `host_guard`, so only
        // authenticated requests reach it.
        .route_layer(middleware::from_fn_with_state(app.clone(), activity::record_actions))
        .merge(web_router(app.cfg.assets.as_deref()))
        .layer(middleware::from_fn_with_state(app.clone(), host_guard))
        .with_state(app.clone())
}

/// Every module's background work: recovery and the sandbox watchers, the queue, loops, red-team
/// runs, the pull-request watch and backfills, notifications, telemetry and the rest. One line per
/// module, kept in alphabetical order; each module's `start_tasks` spawns what it runs.
async fn start_tasks(app: &Shared, router: &Router) {
    crate::autonomy::start_tasks(app);
    crate::burn_down::start_tasks(app);
    crate::gateway::start_tasks(app);
    crate::lifecycle::start_tasks(app);
    crate::loops::start_tasks(app);
    crate::mesh::start_tasks(app).await;
    crate::notify::start_tasks(app);
    crate::publish::start_tasks(app);
    crate::queue::start_tasks(app);
    crate::reclaim::start_tasks(app);
    crate::redteam::start_tasks(app);
    crate::remote::start_tasks(app, router);
    crate::summaries::start_tasks(app);
    crate::telemetry::start_tasks(app);
    crate::version::start_tasks(app);
    crate::watchdog::start_tasks(app);
}

/// The mothership itself: load state from the data dir, serve the API and the web UI, and run the
/// background loops.
pub(crate) async fn serve() -> Result<()> {
    let cfg = Settings::from_env()?;
    // The port first, before anything touches colonies: a second mothership (one started at login
    // while another runs by hand, or the reverse) must stop here, not after running recovery,
    // backfills or reaping against the same data directory.
    let listener = match tokio::net::TcpListener::bind(&cfg.bind).await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("{}", login_item::already_running_message(&cfg.bind));
            // Under launchd/systemd a clean exit keeps the agent from restarting it in a loop.
            if login_item::started_as_login_item() {
                std::process::exit(0);
            }
            bail!("{} is already in use", cfg.bind);
        }
        Err(e) => return Err(e).with_context(|| format!("cannot bind {}", cfg.bind)),
    };
    for dir in ["sessions", "repos", "worktrees", "memory", "plugins"] {
        std::fs::create_dir_all(cfg.data_dir.join(dir))?;
    }
    let (mut sessions, corrupt) = load_sessions(&cfg.data_dir.join("sessions.json"))?;
    for s in &mut sessions {
        if s.org.is_empty() {
            s.org = s.repo.split('/').next().unwrap_or_default().to_string();
        }
    }
    // Colonies persisted as finished while still carrying an attention flag predate the clearing
    // every terminal transition now does; drop those stale flags before `recover` runs, so a
    // stopped colony does not look like it still needs attention.
    let stale_attention = sessions::clear_stale_attention(&mut sessions);
    if stale_attention > 0 {
        println!("sessions: cleared a stale attention flag from {stale_attention} finished colonies");
    }
    let (modules, modules_damage) = ModulesConfig::load(&cfg.config_dir.join("modules.json"))?;
    // Both startup ruin reports show as one alert when both happen: the operator dismisses one
    // banner, not two about the same bad disk.
    let load_damage = match (corrupt, modules_damage) {
        (Some(sessions), Some(modules)) => Some(StorageAlert {
            message: format!("{}\n{}", sessions.message, modules.message),
            ..sessions
        }),
        (sessions, modules) => sessions.or(modules),
    };
    let (agents, agent_problems) = modules::discover_agents(cfg.assets.as_deref());

    // The cockpit API token, minted on first run: every request to the API proves itself with it.
    let api_token = auth::load_or_create(&cfg.config_dir)?;

    // Saved secrets: the system keychain where it answers, the 0600 files otherwise (secrets.rs).
    // The probe can wait on a locked keyring, so it runs off the startup path.
    secrets::install(secrets::Store::new(&cfg.config_dir, secrets::os_backend()));
    std::thread::spawn(|| {
        if let Some(store) = secrets::global() {
            store.probe();
        }
    });

    let boot = Boot {
        sessions,
        modules,
        agents,
        agent_problems,
        load_damage,
        api_token,
    };
    let app = Arc::new(App::new(cfg, boot)?);

    let router = router(&app);

    println!("colonizer listening on http://{}", app.cfg.bind);
    println!("data: {}", app.cfg.data_dir.display());
    match &app.cfg.assets {
        Some(assets) => println!("assets: {}", assets.display()),
        None => println!("assets: not found (run scripts/install.sh)"),
    }
    // The sign-in link: printed always, opened when there is a browser to open in.
    let login_url = auth::login_url(&app.cfg.bind, &app.api_token);
    println!("cockpit: {login_url}");
    if !auth::bind_is_loopback(&app.cfg.bind) {
        eprintln!(
            "warning: colonizer is bound to {}, so the API answers to the network; requests need the API token, but plain HTTP exposes the token to anyone on the path — use a TLS reverse proxy or an SSH tunnel",
            app.cfg.bind
        );
    }
    auth::open_browser(&login_url);
    // The first-run notice: once, while nobody has answered yet, show the exact usage batch on stderr.
    usage::first_run_notice(&app).await;
    match tokio::net::TcpListener::bind(app.cfg.gateway_bind).await {
        Ok(listener) => {
            println!("provider gateway on http://{}", app.cfg.gateway_bind);
            let gateway = gateway::router(app.clone());
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, gateway).await {
                    eprintln!("provider gateway stopped: {e}");
                }
            });
        }
        Err(e) => eprintln!(
            "provider gateway: cannot bind {}: {e}; colonies can't use model providers",
            app.cfg.gateway_bind
        ),
    }

    start_tasks(&app, &router).await;

    tokio::select! {
        result = async { axum::serve(listener, router).await } => result?,
        // microVMs are detached and keep running; sessions reconnect on the next start.
        _ = shutdown_signal() => {
            println!("shutting down; running sessions keep their microVMs");
            app.telemetry.goodbye().await;
            let mesh = app.mesh.lock().await.clone();
            if let Some(mesh) = mesh {
                mesh.shutdown().await;
            }
            // The usage counters flush every few seconds; one last flush loses nothing.
            app.gateway.flush_usage();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream;
    use crate::tests::{temp_root, test_app};
    use axum::routing::{get, post};
    use serde_json::Value;

    /// The real `host_guard` and `status`, a dummy POST route and page: `Router<()>` so tests can
    /// drive it with `oneshot`.
    fn auth_router(app: &Shared) -> Router<()> {
        Router::new()
            .route("/api/status", get(crate::status::status))
            .route("/api/sessions", post(|| async { "created" }))
            .route("/api/stream", get(stream::handler))
            .fallback(|| async { Html("test page") })
            .layer(middleware::from_fn_with_state(app.clone(), host_guard))
            .with_state(app.clone())
    }

    use axum::http::HeaderName;
    use tower::ServiceExt as _;

    async fn body_text(res: Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// A request to the auth test router: loopback Host plus the given headers. The guard runs
    /// before routing, so reaching the dummy POST route proves the guard passed.
    fn guarded(method: Method, uri: &str, headers: Vec<(HeaderName, String)>) -> Request {
        let mut request = Request::builder().method(method).uri(uri);
        request = request.header(header::HOST, "127.0.0.1:7878");
        for (name, value) in headers {
            request = request.header(name, value);
        }
        request.body(axum::body::Body::from("")).unwrap()
    }

    fn bearer(app: &Shared) -> (HeaderName, String) {
        (header::AUTHORIZATION, format!("Bearer {}", app.api_token))
    }

    fn cookie(app: &Shared) -> (HeaderName, String) {
        (header::COOKIE, format!("{}={}", auth::COOKIE_NAME, app.api_token))
    }

    fn origin(value: &str) -> (HeaderName, String) {
        (header::ORIGIN, value.to_string())
    }

    /// The WebSocket handshake headers: their presence is what marks a request as an upgrade.
    fn upgrade() -> Vec<(HeaderName, String)> {
        [
            (header::UPGRADE, "websocket".to_string()),
            (header::CONNECTION, "Upgrade".to_string()),
            (header::SEC_WEBSOCKET_VERSION, "13".to_string()),
            (header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==".to_string()),
        ]
        .to_vec()
    }

    #[tokio::test]
    async fn the_app_files_load_before_sign_in_and_nothing_else_does() {
        let root = temp_root();
        let app = test_app(&root);
        for uri in [
            "/manifest.webmanifest",
            "/sw.js",
            "/sw-routes.js",
            "/offline.html",
            "/icons/icon-192.png",
        ] {
            let res = auth_router(&app).oneshot(guarded(Method::GET, uri, vec![])).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri} is public");
        }
        for uri in ["/", "/assets/index-abc.js", "/icons/../api/sessions", "/sw.js.map"] {
            let res = auth_router(&app).oneshot(guarded(Method::GET, uri, vec![])).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{uri} stays behind sign-in");
        }
        let res = auth_router(&app)
            .oneshot(guarded(Method::POST, "/sw.js", vec![]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "only GET is public");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_manifest_and_service_worker_are_served_with_their_types() {
        let root = temp_root();
        let assets = root.join("assets-dir");
        std::fs::create_dir_all(assets.join("web")).unwrap();
        std::fs::write(assets.join("web/index.html"), "<html></html>").unwrap();
        std::fs::write(assets.join("web/manifest.webmanifest"), "{}").unwrap();
        std::fs::write(assets.join("web/sw.js"), "self;").unwrap();
        let app = test_app(&root);
        let router: Router<()> = web_router(Some(&assets)).with_state(app.clone());
        for (uri, want) in [
            ("/manifest.webmanifest", "application/manifest+json"),
            ("/sw.js", "javascript"),
        ] {
            let res = router.clone().oneshot(guarded(Method::GET, uri, vec![])).await.unwrap();
            let ct = res
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(ct.contains(want), "{uri}: {ct}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn unauthenticated_api_requests_are_rejected_even_with_a_same_origin_origin() {
        let root = temp_root();
        let app = test_app(&root);
        // A correct same-origin Origin used to be enough for scripts; now the token is required.
        for (method, uri) in [
            (Method::POST, "/api/sessions"),
            (Method::GET, "/api/sessions"),
            (Method::GET, "/api/version"),
        ] {
            let res = auth_router(&app)
                .oneshot(guarded(method.clone(), uri, vec![origin("http://127.0.0.1:7878")]))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
            assert!(body_text(res).await.contains("colonizer open"), "{method} {uri}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_bad_host_is_forbidden_even_with_a_valid_token() {
        let root = temp_root();
        let app = test_app(&root);
        let mut req = guarded(Method::POST, "/api/sessions", vec![bearer(&app)]);
        req.headers_mut()
            .insert(header::HOST, HeaderValue::from_static("evil.example"));
        let res = auth_router(&app).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn bearer_requests_pass_without_an_origin_and_fail_with_a_wrong_token() {
        let root = temp_root();
        let app = test_app(&root);
        // No Origin anywhere: the token decides, upgrade or not; without one, told to sign in.
        let token = format!("Bearer {}", app.api_token);
        for (auth, is_upgrade, expected) in [
            (Some(token.clone()), false, StatusCode::OK),
            (Some("Bearer wrong-token".to_string()), false, StatusCode::UNAUTHORIZED),
            (Some(token), true, StatusCode::OK),
            (None, false, StatusCode::UNAUTHORIZED),
            (None, true, StatusCode::UNAUTHORIZED),
        ] {
            let mut headers: Vec<_> = auth.into_iter().map(|auth| (header::AUTHORIZATION, auth)).collect();
            if is_upgrade {
                headers.extend(upgrade());
            }
            let res = auth_router(&app)
                .oneshot(guarded(Method::POST, "/api/sessions", headers))
                .await
                .unwrap();
            assert_eq!(res.status(), expected, "upgrade={is_upgrade}");
            if expected == StatusCode::OK {
                assert_eq!(body_text(res).await, "created");
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cookie_posts_keep_the_same_origin_origin_requirement() {
        let root = temp_root();
        let app = test_app(&root);
        for (value, is_upgrade, expected) in [
            ("http://127.0.0.1:7878", false, StatusCode::OK),
            ("http://evil.example", false, StatusCode::FORBIDDEN),
            ("http://127.0.0.1:7878", true, StatusCode::OK),
            ("http://evil.example", true, StatusCode::FORBIDDEN),
        ] {
            let mut headers = vec![cookie(&app), origin(value)];
            if is_upgrade {
                headers.extend(upgrade());
            }
            let res = auth_router(&app)
                .oneshot(guarded(Method::POST, "/api/sessions", headers))
                .await
                .unwrap();
            assert_eq!(res.status(), expected, "origin {value} upgrade={is_upgrade}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stream_upgrade_without_a_token_is_rejected() {
        let root = temp_root();
        let app = test_app(&root);
        let res = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/stream", upgrade()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stream_cookie_upgrade_from_elsewhere_is_forbidden() {
        let root = temp_root();
        let app = test_app(&root);
        let mut headers = vec![cookie(&app), origin("http://evil.example")];
        headers.extend(upgrade());
        let res = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/stream", headers))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn stream_upgrade_with_a_token_reaches_the_handler() {
        let root = temp_root();
        let app = test_app(&root);
        // Bearer needs no Origin; cookie auth needs the same-origin one. Either way the guard
        // passes and routing reaches the real WebSocket extractor — which answers 426 here only
        // because `oneshot` carries no hyper upgrade state (production answers 101). A guard
        // failure would be 401/403 instead, and a missing route the fallback page.
        let mut cookie_headers = vec![cookie(&app), origin("http://127.0.0.1:7878")];
        cookie_headers.extend(upgrade());
        let mut bearer_headers = vec![bearer(&app)];
        bearer_headers.extend(upgrade());
        for headers in [bearer_headers, cookie_headers] {
            let res = auth_router(&app)
                .oneshot(guarded(Method::GET, "/api/stream", headers))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UPGRADE_REQUIRED);
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_sign_in_link_sets_the_cookie_and_a_bad_token_is_rejected() {
        let root = temp_root();
        let app = test_app(&root);
        let res = auth_router(&app)
            .oneshot(guarded(Method::GET, &format!("/?token={}", app.api_token), vec![]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let set_cookie = res.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
        assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
        assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
        // The token must not linger in caches or leak via the Referer header on the next click.
        assert_eq!(res.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(res.headers().get(header::REFERRER_POLICY).unwrap(), "no-referrer");
        assert!(
            body_text(res).await.contains("location.replace"),
            "the page signs in with JS, not a redirect"
        );

        for uri in ["/", "/?token=wrong"] {
            let res = auth_router(&app).oneshot(guarded(Method::GET, uri, vec![])).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "GET {uri}");
            assert_eq!(res.headers().get(header::CACHE_CONTROL).unwrap(), "no-store", "GET {uri}");
            assert!(body_text(res).await.contains("colonizer open"), "GET {uri}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_public_status_is_an_allowlist_and_the_signed_in_status_is_not() {
        let root = temp_root();
        let app = test_app(&root);
        let public = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/status", vec![]))
            .await
            .unwrap();
        assert_eq!(public.status(), StatusCode::OK);
        let public_text = body_text(public).await;
        let public_body: Value = serde_json::from_str(&public_text).unwrap();
        let mut keys: Vec<&str> = public_body.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["host", "queue_depth", "runtime", "storage", "version"]);

        let signed_in = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/status", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(signed_in.status(), StatusCode::OK);
        let full: Value = serde_json::from_str(&body_text(signed_in).await).unwrap();
        for key in ["github", "claude", "assets", "modules", "sandbox", "mesh"] {
            assert!(full.get(key).is_some(), "the signed-in body keeps {key}");
            assert!(public_body.get(key).is_none(), "the public body drops {key}");
        }
        // Whatever the probes found — the host id, the hostname — must not cross over.
        let id = full["host"]["id"].as_str().unwrap().to_string();
        assert!(!public_text.contains(&id), "the host id stays in the signed-in body");
        if let Some(hostname) = full["host"]["hostname"].as_str() {
            assert!(!public_text.contains(hostname), "the hostname stays in the signed-in body");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod route_table_tests;
