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
    App, Shared, StorageAlert, activity, api_tokens, auth, client_error, gateway, login_item, modules, remote, secrets, sessions,
    usage,
};
use anyhow::{Context, Result, bail};
use axum::{
    Router,
    extract::{MatchedPath, Request, State},
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
    let cookie = auth::cookie_token(req.headers());
    let cookie_ok = cookie
        .as_deref()
        .is_some_and(|token| auth::tokens_match(token, &app.api_token));
    // A paired phone's own cookie (phone.rs, issue #746): the cockpit like the owner's cookie, minus
    // the routes that mint, approve or revoke access. Revoking the phone ends it on the next request.
    let phone = match (bearer_ok || cookie_ok, cookie.as_deref()) {
        (false, Some(token)) => app.phones.authenticate(token),
        _ => None,
    };
    if phone.is_some() && !crate::phone::phone_may(req.method(), req.uri().path()) {
        return (
            StatusCode::FORBIDDEN,
            "a phone cannot change access; use the cockpit on your computer",
        )
            .into_response();
    }
    if bearer_ok || cookie_ok || phone.is_some() {
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
        if let Some(phone) = phone {
            let key = phone.revocation_key();
            req.extensions_mut().insert(phone);
            return run_revocable(&key, req, next).await;
        }
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
            // On the UHP surface the same verdicts wear the §7.7 envelope (issue #650).
            if crate::uhp::is_uhp(req.uri().path()) {
                return crate::uhp::denied(deny);
            }
            return deny.into_response();
        }
        req.extensions_mut().insert(auth::Authenticated(true));
        req.extensions_mut().insert(auth::Via::Token(scoped.name.clone()));
        let key = format!("token:{}", scoped.id);
        req.extensions_mut().insert(scoped);
        return run_revocable(&key, req, next).await;
    }
    // No valid token: the reduced status, the sign-in link's cookie, or how to sign in.
    let path = req.uri().path().to_string();
    // The UHP surface answers its own refusals as envelopes, not pages (issue #650). Discovery is
    // how a client finds out this is a UHP server at all, so it is served before any credential
    // check; every other `/uhp` path gets the envelope's authentication error.
    if crate::uhp::is_uhp(&path) {
        // `HEAD` too, because axum's `get` serves it and a client may probe before it reads.
        if matches!(*req.method(), Method::GET | Method::HEAD) && path == crate::uhp::DISCOVERY_PATH {
            req.extensions_mut().insert(auth::Authenticated(false));
            return next.run(req).await;
        }
        return crate::uhp::unauthenticated();
    }
    if path == "/api" || path.starts_with("/api/") {
        if req.method() == Method::GET && path == "/api/status" {
            req.extensions_mut().insert(auth::Authenticated(false));
            return next.run(req).await;
        }
        // The fleet pairing's two open doors (fleet_members.rs): their whole authentication is
        // what the body carries — a single-use invite code, a pairing id plus the nonce only its
        // joiner holds — so a request with no token is admitted to exactly those two.
        if req.method() == Method::POST && (path == "/api/fleet/peer/redeem" || path.starts_with("/api/fleet/peer/pairings/")) {
            req.extensions_mut().insert(auth::Authenticated(false));
            return next.run(req).await;
        }
        // Answering a question straight from its push (issue #742): the one-shot token in the body
        // is the whole credential, so this one method+path is admitted with no cookie or bearer —
        // anything but a live minted token answers 401 in `answer_tokens::answer`.
        if req.method() == Method::POST && path == "/api/push/answer" {
            req.extensions_mut().insert(auth::Authenticated(false));
            return next.run(req).await;
        }
        // The phone pairing page's poll (phone.rs): its authentication is the pairing cookie only
        // the browser that spent the invite holds, checked and rate-limited by the route itself.
        if req.method() == Method::POST && path == "/api/phone/claim" {
            req.extensions_mut().insert(auth::Authenticated(false));
            return next.run(req).await;
        }
        // A fleet member the owner removed presents a token revoked on purpose: it reads 403
        // "removed from the fleet", so the member can tell removal from a bad credential.
        if let Some(token) = auth::bearer_token(req.headers())
            && app.fleet_members.is_removed_token(&token).await
        {
            return crate::client_error(StatusCode::FORBIDDEN, "removed from the fleet").into_response();
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
    // A phone's scanned invite (`/?pair=…`, phone.rs, issue #746). Spent here on its first
    // presentation, which binds the pairing to this browser and shows the code to confirm in the
    // local cockpit; no credential is handed over yet. A wrong, spent or expired invite falls
    // through to the locked page, saying nothing about why, and counts against the rate limit.
    // Already-authenticated requests never reach this, so they never spend an invite.
    if req.method() == Method::GET
        && let Some(code) = auth::query_param(req.uri().query(), "pair")
    {
        let user_agent = req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        match app.phones.open(&code, user_agent) {
            Ok(Some(opened)) => return crate::phone::pairing_response(&opened),
            Ok(None) => {}
            Err(crate::phone::Limited) => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "too many failed pairing attempts; wait a minute",
                )
                    .into_response();
            }
        }
    }
    let mut res = (StatusCode::UNAUTHORIZED, Html(auth::locked_page())).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// Runs a request a revocable credential authenticated — a paired phone or a scoped API token —
/// so that revoking it takes effect at once (issue #746): a request that arrives already revoked,
/// or is revoked while in flight, is a 401; a streamed response body ends the moment it is revoked;
/// and the handlers that upgrade to a socket take the [`auth::Revocation`] from the request and
/// close the socket when it fires.
async fn run_revocable(key: &str, mut req: Request, next: Next) -> Response {
    let revoked = || (StatusCode::UNAUTHORIZED, auth::UNAUTHORIZED_BODY).into_response();
    let revocation = auth::Revocation::watch(key);
    if revocation.is_fired() {
        return revoked();
    }
    req.extensions_mut().insert(revocation.clone());
    let res = tokio::select! {
        res = next.run(req) => res,
        () = revocation.fired() => return revoked(),
    };
    if res.status() == StatusCode::SWITCHING_PROTOCOLS {
        return res; // the socket's own handler holds the revocation from here
    }
    let (parts, body) = res.into_parts();
    let ended = Box::pin(async move { revocation.fired().await });
    let body = axum::body::Body::from_stream(futures_util::StreamExt::take_until(body.into_data_stream(), ended));
    Response::from_parts(parts, body)
}

/// The PWA files served before sign-in: the manifest, the service worker, its routing script, its
/// offline outbox, the offline page and the icons. Nothing under `/assets` or `/api`.
fn is_public_app_file(path: &str) -> bool {
    matches!(
        path,
        "/manifest.webmanifest" | "/sw.js" | "/sw-routes.js" | "/sw-outbox.js" | "/offline.html"
    ) || (path.starts_with("/icons/") && !path.contains("..") && path.len() < 64)
}

/// Which signal asked the process to stop: Ctrl-C (an operator at the terminal) or SIGTERM (a
/// service manager, `kill`, a deploy). Only SIGTERM drains first — see `serve`.
enum Shutdown {
    Interrupt,
    Terminate,
}

async fn shutdown_signal() -> Shutdown {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => Shutdown::Interrupt,
        _ = term.recv() => Shutdown::Terminate,
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

/// Every `/api` path no module route claims, answered as the API error it is (`#641`): before,
/// it fell through to the SPA fallback and read as a 200 `text/html` success, so a probe for an
/// unknown route looked like a page. The `/uhp` prefix gets the same fence in the §7.7 envelope
/// (issue #651): a protocol probe that misses — including the un-normalised and percent-encoded
/// `..` segments a router never matches — must read as a JSON 404, never as the cockpit's page.
/// The API fence is segment-aware — `/apiary` is a cockpit
/// path — and a matched route never reaches this: the router stamps `MatchedPath` on what it
/// matched, and only a request that fell to a fallback gets here without one. Which is also the
/// invariant that API routes stay registered flat: a router `nest`ed under `/api` carries only
/// `MatchedNestedPath`, so its real routes would read as unmatched and be answered 404. It
/// layers the web fallback too, so it needs no web dir and works the same when the UI was never
/// built.
async fn api_not_found(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let unmatched = req.extensions().get::<MatchedPath>().is_none();
    if (path == "/api" || path.starts_with("/api/")) && unmatched {
        client_error(StatusCode::NOT_FOUND, "no such API route").into_response()
    } else if crate::uhp::is_uhp(path) && unmatched {
        crate::uhp::unmatched(path)
    } else {
        next.run(req).await
    }
}

/// Every API route, before any layer: `router` wraps them in the activity log's route layer and
/// `host_guard`. Each module keeps its own routes in its `routes()`.
pub(crate) fn api_routes() -> Router<Shared> {
    // One line per module that serves API routes, kept in alphabetical order.
    Router::new()
        .merge(crate::activity::routes())
        .merge(crate::answer_tokens::routes())
        .merge(crate::api_tokens::routes())
        .merge(crate::archive::routes())
        .merge(crate::autonomy::routes())
        .merge(crate::burn_down::routes())
        .merge(crate::chat::routes())
        .merge(crate::chat_images::routes())
        .merge(crate::claude_accounts::routes())
        .merge(crate::claude_login::routes())
        .merge(crate::code::routes())
        .merge(crate::colonize::routes())
        .merge(crate::colony_secrets::routes())
        .merge(crate::deja::routes())
        .merge(crate::deps::routes())
        .merge(crate::docs_loop::routes())
        .merge(crate::drain::routes())
        .merge(crate::egress::routes())
        .merge(crate::findings::routes())
        .merge(crate::fleet::routes())
        .merge(crate::fleet_history::routes())
        .merge(crate::fleet_members::routes())
        .merge(crate::fleet_policy::routes())
        .merge(crate::fleet_sync::routes())
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
        .merge(crate::merge_loop::routes())
        .merge(crate::merge_train::routes())
        .merge(crate::supply_chain_loop::routes())
        .merge(crate::switch_agent::routes())
        .merge(crate::modules::routes())
        .merge(crate::notify::routes())
        .merge(crate::orgs::routes())
        .merge(crate::packages::routes())
        .merge(crate::phone::routes())
        .merge(crate::plugins::routes())
        .merge(crate::previews::routes())
        .merge(crate::providers::routes())
        .merge(crate::publish::routes())
        .merge(crate::quota_cards::routes())
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
        .merge(crate::transcript::routes())
        .merge(crate::ts_any_loop::routes())
        .merge(crate::uhp::routes())
        .merge(crate::update::routes())
        .merge(crate::upload::routes())
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
        // Inside `host_guard`, so an unauthenticated caller still hits the 401 wall, not the 404.
        .layer(middleware::from_fn(api_not_found))
        .layer(middleware::from_fn_with_state(app.clone(), host_guard))
        .with_state(app.clone())
}

/// Every module's background work: recovery and the sandbox watchers, the queue, loops, red-team
/// runs, the pull-request watch and backfills, notifications, telemetry and the rest. One line per
/// module, kept in alphabetical order; each module's `start_tasks` spawns what it runs.
async fn start_tasks(app: &Shared, router: &Router) {
    crate::autonomy::start_tasks(app);
    crate::burn_down::start_tasks(app);
    crate::docs_loop::start_tasks(app);
    crate::fleet_sync::start_tasks(app);
    crate::gateway::start_tasks(app);
    crate::lifecycle::start_tasks(app);
    crate::loops::start_tasks(app);
    crate::merge_loop::start_tasks(app);
    crate::merge_train::start_tasks(app);
    crate::supply_chain_loop::start_tasks(app);
    crate::mesh::start_tasks(app).await;
    crate::notify::start_tasks(app);
    crate::publish::start_tasks(app);
    crate::queue::start_tasks(app);
    crate::reclaim::start_tasks(app);
    crate::redteam::start_tasks(app);
    crate::remote::start_tasks(app, router);
    crate::summaries::start_tasks(app);
    crate::telemetry::start_tasks(app);
    crate::ts_any_loop::start_tasks(app);
    crate::usage::start_tasks(app);
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
    // The one store this run reads and writes through (docs/session-store.md): built once here and
    // threaded into startup and the `App`, so every later save and append answers by the same name.
    let store: Arc<dyn crate::store::SessionStore> = Arc::new(crate::store::LocalDirStore::new(cfg.data_dir.clone()));
    let (mut sessions, corrupt) = load_sessions(store.as_ref(), &cfg.data_dir.join("sessions.json")).await?;
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
        store,
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
        signal = shutdown_signal() => {
            // Issue #880: a SIGTERM (systemd stop, a deploy's `kill`) must not cut a boot or a
            // publish short, so drain first — bounded by the same timeout the update uses. Ctrl-C
            // is the operator asking for the process to stop now, and exits promptly.
            if matches!(signal, Shutdown::Terminate) {
                let timeout = crate::drain::timeout();
                println!("SIGTERM: draining in-flight colonies (up to {timeout:?}) before exit");
                if crate::drain::drain_and_wait(&app, timeout).await {
                    println!("drained; nothing was left booting or publishing");
                } else {
                    eprintln!("drain timed out; exiting with colonies still in flight — they are requeued on the next start");
                }
            }
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
    use crate::tests::{temp_root, test_app, test_app_with};
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
            "/sw-outbox.js",
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
    async fn a_colony_gateway_token_is_not_an_api_token() {
        let root = temp_root();
        let app = test_app(&root);
        // The token boot issues the colony for its gateway routes (gateway.rs checks it against the
        // session dir) is not a credential for the cockpit API: presented as a Bearer there it is
        // just an unauthenticated request, on a loopback Host with no Origin.
        let token = crate::util::random_token();
        std::fs::create_dir_all(app.session_dir("c1")).unwrap();
        std::fs::write(app.gateway_token_file("c1"), token.as_bytes()).unwrap();
        let res = auth_router(&app)
            .oneshot(guarded(
                Method::POST,
                "/api/sessions",
                vec![(header::AUTHORIZATION, format!("Bearer {token}"))],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(body_text(res).await.contains("colonizer open"));
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

    /// Issue #746, end to end through the guard: an invite opens one pairing on one browser, hands
    /// nothing over until the local cockpit confirms the code, then gives that phone a credential of
    /// its own — never the API token — which cannot manage access and is revoked on its own.
    #[tokio::test]
    async fn a_phone_pairs_through_a_confirmed_code_and_is_revocable() {
        let root = temp_root();
        let app = test_app(&root);
        use serde_json::json;
        let router = || {
            Router::new()
                .route("/api/sessions", post(|| async { "created" }))
                .merge(crate::phone::routes())
                .fallback(|| async { Html("test page") })
                .layer(middleware::from_fn_with_state(app.clone(), host_guard))
                .with_state(app.clone())
        };
        let same_origin = || origin("http://127.0.0.1:7878");
        let json_type = || (header::CONTENT_TYPE, "application/json".to_string());
        let set_cookie = |res: &Response, name: &str| {
            res.headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .find(|v| v.starts_with(&format!("{name}=")) && !v.contains("Max-Age=0"))
                .map(|v| v.split(';').next().unwrap().to_string())
        };

        // The owner mints an invite; the QR carries it, never the API token.
        let res = router()
            .oneshot(guarded(Method::POST, "/api/phone/invites", vec![cookie(&app), same_origin()]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let invite: Value = serde_json::from_str(&body_text(res).await).unwrap();
        let code = invite["code"].as_str().unwrap().to_string();
        assert_ne!(code, app.api_token);
        assert_eq!(invite["ttl_secs"], 300);

        // The phone opens it: a pairing page with the confirm code, a device cookie, no credential.
        let res = router()
            .oneshot(guarded(
                Method::GET,
                &format!("/?pair={code}"),
                vec![(header::USER_AGENT, "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0)".into())],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
        let pair = set_cookie(&res, crate::phone::PAIR_COOKIE).expect("the device cookie");
        assert!(
            set_cookie(&res, auth::COOKIE_NAME).is_none(),
            "no credential before the confirm"
        );
        let page = body_text(res).await;
        assert!(!page.contains(&app.api_token));
        let confirm_code = page
            .split("aria-label=\"Confirmation code\">")
            .nth(1)
            .and_then(|rest| rest.split('<').next())
            .unwrap()
            .to_string();
        assert_eq!(confirm_code.len(), 7, "{confirm_code}");

        // Single use: a second browser presenting the same invite gets the locked page.
        let res = router()
            .oneshot(guarded(Method::GET, &format!("/?pair={code}"), vec![]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(set_cookie(&res, crate::phone::PAIR_COOKIE).is_none());

        let claim = |with: Option<&str>| {
            let headers = with.map(|c| vec![(header::COOKIE, c.to_string())]).unwrap_or_default();
            router().oneshot(guarded(Method::POST, "/api/phone/claim", headers))
        };
        assert_eq!(
            claim(Some(&pair)).await.unwrap().status(),
            StatusCode::ACCEPTED,
            "waiting for the owner"
        );
        assert_eq!(
            claim(None).await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "no device cookie, nothing"
        );

        // Confirming is the owner's, in the local cockpit — and a wrong code approves nothing.
        let digits: String = confirm_code.chars().filter(char::is_ascii_digit).collect();
        let wrong = if digits == "000000" { "111111" } else { "000000" };
        let confirm = |code: &str| {
            router().oneshot({
                let mut req = guarded(
                    Method::POST,
                    "/api/phone/pairings/confirm",
                    vec![cookie(&app), same_origin(), json_type()],
                );
                *req.body_mut() = axum::body::Body::from(json!({"code": code}).to_string());
                req
            })
        };
        assert_eq!(confirm(wrong).await.unwrap().status(), StatusCode::NOT_FOUND);
        assert_eq!(claim(Some(&pair)).await.unwrap().status(), StatusCode::ACCEPTED);
        assert_eq!(confirm(&confirm_code).await.unwrap().status(), StatusCode::OK);

        // The claim hands the phone its own credential, once.
        let res = claim(Some(&pair)).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let phone_cookie = set_cookie(&res, auth::COOKIE_NAME).unwrap();
        assert!(
            phone_cookie.starts_with(&format!("{}=cph_", auth::COOKIE_NAME)),
            "{phone_cookie}"
        );
        assert!(!phone_cookie.contains(&app.api_token));
        assert_eq!(
            claim(Some(&pair)).await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "claimed once"
        );

        // The phone runs the cockpit, but cannot mint or approve access.
        let as_phone =
            |method: Method, uri: &str| guarded(method, uri, vec![(header::COOKIE, phone_cookie.clone()), same_origin()]);
        let res = router().oneshot(as_phone(Method::GET, "/api/phone")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let listed: Value = serde_json::from_str(&body_text(res).await).unwrap();
        let device = listed["devices"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(listed["devices"][0]["label"], "iPhone");
        assert_eq!(
            router()
                .oneshot(as_phone(Method::POST, "/api/sessions"))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            router()
                .oneshot(as_phone(Method::POST, "/api/phone/invites"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            router()
                .oneshot(as_phone(Method::DELETE, &format!("/api/phone/devices/{device}")))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        // The owner revokes it, and it is signed out on its next request.
        let res = router()
            .oneshot(guarded(
                Method::DELETE,
                &format!("/api/phone/devices/{device}"),
                vec![cookie(&app), same_origin()],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            router().oneshot(as_phone(Method::GET, "/api/phone")).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #746: revoking takes effect at once. A paired phone's open colony events socket, and a
    /// scoped API token's, close within a second of the revoke, and the same credential cannot
    /// open another.
    #[tokio::test]
    async fn revoking_a_phone_or_a_token_closes_its_open_sockets_at_once() {
        use futures_util::StreamExt as _;
        use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue as WsValue};
        let (app, root) = crate::sessions::tests::app_with_colony("abc", crate::sessions::SessionStatus::Running).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = router(&app);
        let server = tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
        let open = |header: (&'static str, String)| async move {
            let mut req = format!("ws://{addr}/api/sessions/abc/events").into_client_request().unwrap();
            req.headers_mut().insert(header.0, WsValue::from_str(&header.1).unwrap());
            req.headers_mut()
                .insert("origin", WsValue::from_str(&format!("http://{addr}")).unwrap());
            tokio_tungstenite::connect_async(req).await
        };
        // The socket ends — a close frame, an error or the end of the stream — within a second.
        async fn closes(
            mut socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        ) -> bool {
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    match socket.next().await {
                        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | Some(Err(_)) | None => return,
                        Some(Ok(_)) => continue,
                    }
                }
            })
            .await
            .is_ok()
        }

        // A paired phone.
        let phone_token = app.phones.add("iPhone").unwrap();
        let device = app.phones.view()["devices"][0]["id"].as_str().unwrap().to_string();
        let phone_cookie = ("cookie", format!("{}={phone_token}", auth::COOKIE_NAME));
        let (socket, _) = open(phone_cookie.clone()).await.expect("the phone opens the events socket");
        assert!(app.phones.revoke(&device).is_some());
        assert!(
            closes(socket).await,
            "the phone's socket closed within a second of the revoke"
        );
        assert!(open(phone_cookie).await.is_err(), "and the phone cannot open another");

        // A scoped API token, the same way.
        let made = app
            .api_tokens
            .create(crate::api_tokens::NewToken {
                name: "watcher".into(),
                scope: "read".into(),
                orgs: Vec::new(),
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            })
            .await
            .unwrap();
        let bearer = ("authorization", format!("Bearer {}", made.token));
        let (socket, _) = open(bearer.clone()).await.expect("the token opens the events socket");
        assert!(app.api_tokens.revoke(&made.meta.id).await.is_some());
        assert!(
            closes(socket).await,
            "the token's socket closed within a second of the revoke"
        );
        assert!(open(bearer).await.is_err(), "and the token cannot open another");

        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }

    /// A request still in flight when its credential is revoked fails, and a streamed body stops.
    #[tokio::test]
    async fn a_revoked_credentials_in_flight_request_fails() {
        let root = temp_root();
        let app = test_app(&root);
        let token = app.phones.add("iPhone").unwrap();
        let device = app.phones.view()["devices"][0]["id"].as_str().unwrap().to_string();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let started = std::sync::Arc::new(std::sync::Mutex::new(Some(started_tx)));
        let slow = Router::new()
            .route(
                "/api/slow",
                get(move || {
                    let started = started.clone();
                    async move {
                        if let Some(tx) = started.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        "done"
                    }
                }),
            )
            .layer(middleware::from_fn_with_state(app.clone(), host_guard))
            .with_state(app.clone());
        let request = guarded(
            Method::GET,
            "/api/slow",
            vec![(header::COOKIE, format!("{}={token}", auth::COOKIE_NAME))],
        );
        let in_flight = tokio::spawn(slow.oneshot(request));
        started_rx.await.unwrap();
        app.phones.revoke(&device).unwrap();
        let res = tokio::time::timeout(std::time::Duration::from_secs(1), in_flight)
            .await
            .expect("answered at once")
            .unwrap()
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
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
        assert_eq!(keys, ["host", "queue_depth", "runner", "runtime", "storage", "version"]);
        // Issue #764: the queue loop's age crosses over; `fleet_sync` only on a fleet member.
        assert!(public_body["runner"].get("last_tick_age_s").is_some());
        app.fleet_members
            .set_membership_for_tests(Some(crate::fleet_sync::Target {
                owner_url: "http://owner.example:7878".into(),
                member_id: "mem_1".into(),
                token: "col_secret_member_token".into(),
            }))
            .await;
        let member = auth_router(&app)
            .oneshot(guarded(Method::GET, "/api/status", vec![]))
            .await
            .unwrap();
        let member_text = body_text(member).await;
        let member_body: Value = serde_json::from_str(&member_text).unwrap();
        assert_eq!(member_body["fleet_sync"]["state"], "consent_required", "{member_body}");
        assert_eq!(member_body["fleet_sync"]["consent"], false);
        assert!(
            !member_text.contains("owner.example") && !member_text.contains("col_secret"),
            "no URL or token crosses over"
        );
        app.fleet_members.set_membership_for_tests(None).await;

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

    /// Unknown `/api` paths answer the JSON 404 an API caller reads, and nothing else changes:
    /// registered routes still match, `/apiary` is still a cockpit path, and `/assets` still 404s
    /// on a missing chunk (#641).
    #[tokio::test]
    async fn unknown_api_paths_answer_a_json_404_and_cockpit_paths_keep_the_spa() {
        let root = temp_root();
        let assets = root.join("assets-dir");
        std::fs::create_dir_all(assets.join("web/assets")).unwrap();
        std::fs::write(assets.join("web/index.html"), "<html>cockpit</html>").unwrap();
        std::fs::write(assets.join("web/assets/index-abc.js"), "export {};").unwrap();
        let app = test_app_with(&root, |cfg| cfg.assets = Some(assets.clone()));
        let router = router(&app);

        for uri in ["/api", "/api/", "/api/does-not-exist", "/api/does/not/exist"] {
            for method in [Method::GET, Method::POST] {
                let res = router
                    .clone()
                    .oneshot(guarded(method.clone(), uri, vec![bearer(&app)]))
                    .await
                    .unwrap();
                assert_eq!(res.status(), StatusCode::NOT_FOUND, "{method} {uri}");
                assert_eq!(
                    res.headers().get(header::CONTENT_TYPE).unwrap(),
                    "application/json",
                    "{method} {uri}"
                );
                let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
                assert_eq!(body["error"], "no such API route", "{method} {uri}");
            }
        }

        // A registered route still wins over the fallback, from the same router.
        let res = router
            .clone()
            .oneshot(guarded(Method::GET, "/api/status", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "/api/status still answers");
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json",
            "/api/status is still the API"
        );

        // A wrong method on a real route is that route's 405, not the fallback's 404: the match
        // counts, the method does not.
        let res = router
            .clone()
            .oneshot(guarded(Method::POST, "/api/status", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED, "POST /api/status");

        // And the 404 sits inside the token wall: unknown or not, an `/api` request without a
        // token reads as 401, like every other API route.
        let res = router
            .clone()
            .oneshot(guarded(Method::GET, "/api/does-not-exist", vec![]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "unknown /api stays behind sign-in");
        assert!(body_text(res).await.contains("colonizer open"), "told to sign in");

        // `/apiary` is not `/api`: the cockpit's SPA fallback answers it as a page.
        let res = router
            .clone()
            .oneshot(guarded(Method::GET, "/apiary", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "/apiary is a page");
        let page = body_text(res).await;
        assert!(
            page.contains("cockpit") && !page.contains("no such"),
            "/apiary serves the SPA"
        );

        // Hashed build assets keep their own 404: a missing chunk is not the page.
        let res = router
            .clone()
            .oneshot(guarded(Method::GET, "/assets/index-abc.js", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "a present chunk is served");
        let res = router
            .oneshot(guarded(Method::GET, "/assets/missing.js", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "a missing chunk still 404s");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Without a built web dir the API 404 is the same, and only the cockpit falls back to the
    /// "not built" page (#641).
    #[tokio::test]
    async fn unknown_api_paths_are_a_json_404_even_without_a_web_dir() {
        let root = temp_root();
        let app = test_app(&root);
        let router = router(&app);

        let res = router
            .clone()
            .oneshot(guarded(Method::GET, "/api/does-not-exist", vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(res.headers().get(header::CONTENT_TYPE).unwrap(), "application/json");
        let body: Value = serde_json::from_str(&body_text(res).await).unwrap();
        assert_eq!(body["error"], "no such API route");

        let res = router.oneshot(guarded(Method::GET, "/", vec![bearer(&app)])).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "/ still answers");
        assert!(
            body_text(res).await.contains("Colonizer is running"),
            "/ is the not-built page"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Every `/uhp` path no route claims answers a JSON 404 in the §7.7 envelope, never the
    /// cockpit's page (issue #651) — including the traversal probes the conformance suite sends,
    /// whose `..` segments no router matches and whose encoded ones name no artifact. The body
    /// never carries file contents, and without a token the wall stays the wall.
    #[tokio::test]
    async fn unknown_uhp_paths_answer_a_json_404_never_the_spa() {
        let root = temp_root();
        let app = test_app(&root);
        let router = router(&app);

        for uri in [
            "/uhp",
            "/uhp/",
            "/uhp/does-not-exist",
            "/uhp/v1/does-not-exist",
            // Un-normalised traversal: the dots are literal path segments, so nothing matches.
            "/uhp/v1/containers/cntr_x/files/../../etc/passwd/content",
        ] {
            let res = router
                .clone()
                .oneshot(guarded(Method::GET, uri, vec![bearer(&app)]))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "GET {uri}");
            assert_eq!(
                res.headers().get(header::CONTENT_TYPE).unwrap(),
                "application/json",
                "GET {uri}"
            );
            let text = body_text(res).await;
            let body: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(body["error"]["type"], "invalid_request_error", "GET {uri}: {text}");
            assert!(body["error"]["code"].is_string(), "GET {uri}: {text}");
            assert!(!text.contains("root:"), "GET {uri} leaks no file");
        }

        // The encoded traversal does match the artifact route — one segment, decoded by the
        // extractor — and is refused there: an unknown container first, and the name would be
        // `file_not_found`. Either way a JSON 404 that carries no file contents.
        let uri = "/uhp/v1/containers/cntr_x/files/..%2f..%2fetc%2fpasswd/content";
        let res = router
            .clone()
            .oneshot(guarded(Method::GET, uri, vec![bearer(&app)]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "GET {uri}");
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json",
            "GET {uri}"
        );
        let text = body_text(res).await;
        assert!(!text.contains("root:"), "GET {uri} leaks no file: {text}");
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["error"]["code"], "session_not_found", "GET {uri}: {body}");

        // Without a token the /uhp surface sits behind sign-in like every other route but
        // discovery — a 401, in the envelope a protocol client can read (issue #650), never the page.
        let res = router
            .oneshot(guarded(Method::GET, "/uhp/does-not-exist", vec![]))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let text = body_text(res).await;
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["error"]["type"], "authentication_error", "{text}");
        let _ = std::fs::remove_dir_all(root);
    }
}

#[cfg(test)]
mod route_table_tests;
