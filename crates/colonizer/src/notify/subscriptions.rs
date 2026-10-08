//! Webhook subscriptions (issue #899): more receivers than the one owner URL, each registered
//! through the API — by the owner, or by a scoped API token at `operate` or `launch` scope for its
//! own automations.
//!
//! A subscription is `{url, events, secret}`. It gets the same payload, id and headers the notify
//! module's own webhook gets (docs/protocol/webhooks.md), signed with its own secret, through the
//! same outbox — so with the same retries and dead letter (issue #898). What it can see is the
//! point of the issue: a subscription made by a scoped token only ever receives events about
//! colonies inside that token's org and repo limits, checked against the token as it is at delivery
//! time, so narrowing or revoking the token narrows or ends the subscription at once. Host-level
//! events (a provider, the judge, an account, the digest) name no colony and belong to no scope, so
//! only the owner's subscriptions get them.
//!
//! A scoped token sees and deletes only its own subscriptions; the owner sees and deletes all of
//! them. The secret is write-only — it is stored on the mothership (mode 0600) and never answered —
//! and it is required (issue #900): every delivery carries a signature, so a subscription without
//! one is refused at the API rather than left to fail on every attempt.

use super::{EVENT_NAMES, LIFECYCLE_EVENTS, outbox, webhook_valid};
use crate::{
    ApiResult, App, Shared,
    api_tokens::{Need, Scope, ScopedToken},
    client_error,
    sessions::Session,
    util::{short_id, write_private},
};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{Method, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{net::IpAddr, path::PathBuf, sync::LazyLock};
use tokio::sync::Mutex;

/// The routes, their scoped-token rule and their activity, registered in `features::ALL`.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "webhooks",
    routes,
    token_scope: Some(token_scope),
    activity: ACTIVITY,
    kinds: &[],
    start_tasks: None,
};

fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/webhooks", routing::get(list).post(create))
        .route("/api/webhooks/{id}", routing::delete(remove))
}

/// Managing a subscription needs `operate`: it is driving the token's own automation, not watching.
/// The handlers keep each token to its own subscriptions.
fn token_scope<'a>(method: &Method, segs: &[&'a str]) -> Option<Need<'a>> {
    match segs {
        ["api", "webhooks"] if *method == Method::GET || *method == Method::POST => Some(Need::Bare(Scope::Operate)),
        ["api", "webhooks", id] if *method == Method::DELETE && !id.is_empty() => Some(Need::Bare(Scope::Operate)),
        _ => None,
    }
}

const ACTIVITY: &[crate::activity::Rule] = &[
    crate::activity::rule(
        "POST",
        "/api/webhooks",
        "settings.save",
        crate::activity::Target::Fixed("a webhook subscription", "notifications"),
    ),
    crate::activity::rule(
        "DELETE",
        "/api/webhooks/{id}",
        "settings.remove",
        crate::activity::Target::Fixed("a webhook subscription", "notifications"),
    ),
];

/// The most subscriptions one scoped token may hold, and the most in all.
const MAX_PER_TOKEN: usize = 20;
const MAX_TOTAL: usize = 200;
/// The longest webhook address accepted.
const MAX_URL: usize = 2048;

/// The subscriptions file's one writer at a time.
static STORE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// One subscription as stored. The secret is here and nowhere else.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub url: String,
    /// The event names it wants; empty means every event in its scope.
    #[serde(default)]
    pub events: Vec<String>,
    /// Required (issue #900), but read as an option: a subscription stored before that requirement
    /// has none, and the outbox refuses to send to it rather than sending it unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
    /// The scoped token that made it, or `None` for the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Subscription {
    /// What the API answers: never the secret, only whether there is one.
    fn view(&self) -> Value {
        json!({
            "id": self.id,
            "url": self.url,
            "events": self.events,
            "has_secret": self.secret.is_some(),
            "token": self.token,
            "created_at": self.created_at,
        })
    }

    /// The outbox target a delivery to this subscription carries.
    fn target(&self) -> String {
        format!("{TARGET_PREFIX}{}", self.id)
    }

    fn wants(&self, event: &str) -> bool {
        self.events.is_empty() || self.events.iter().any(|e| e == event)
    }
}

/// The outbox target prefix for a subscription: `sub:<id>`.
pub(super) const TARGET_PREFIX: &str = "sub:";

fn file(app: &App) -> PathBuf {
    app.cfg.config_dir.join("webhook-subscriptions.json")
}

/// The subscriptions as saved; a missing file is none, and an unreadable one is logged and read as
/// none rather than stopping notifications.
fn load(app: &App) -> Vec<Subscription> {
    let path = file(app);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!(
                "notify: {} could not be parsed ({e}); no webhook subscriptions",
                path.display()
            );
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

/// Saves the subscriptions, mode 0600 since they hold signing secrets, renamed into place.
fn save(app: &App, list: &[Subscription]) -> anyhow::Result<()> {
    let path = file(app);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_file_name(format!("webhook-subscriptions.json.{}.tmp", short_id()));
    write_private(&tmp, &serde_json::to_vec_pretty(list)?)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// Adds one subscription directly, for tests that point one at a receiver on 127.0.0.1, which the
/// API refuses to a scoped token.
#[cfg(test)]
pub(crate) async fn insert(app: &App, url: &str, events: &[&str], secret: Option<&str>, token: Option<&str>) -> String {
    let _guard = STORE.lock().await;
    let mut list = load(app);
    let id = format!("whs_{}", short_id());
    list.push(Subscription {
        id: id.clone(),
        url: url.into(),
        events: events.iter().map(|e| e.to_string()).collect(),
        secret: secret.map(String::from),
        token: token.map(String::from),
        created_at: Utc::now(),
    });
    save(app, &list).unwrap();
    id
}

/// The signing secret of the subscription an outbox target names: `None` when the subscription is
/// gone — the delivery is then dropped — and `Some(None)` when it has no secret.
pub(super) fn secret_for(app: &App, target: &str) -> Option<Option<String>> {
    let id = target.strip_prefix(TARGET_PREFIX)?;
    load(app).into_iter().find(|s| s.id == id).map(|s| s.secret)
}

/// Whether a scoped token may receive an event about `colony` (`None`: a host-level event). The
/// token is read as it is now: one revoked, narrowed below `operate`, or limited away from the
/// colony's org or repository gets nothing.
fn in_scope(token: Option<&ScopedToken>, colony: Option<&Session>) -> bool {
    let Some(token) = token else { return false };
    let Some(colony) = colony else { return false };
    token.scope >= Scope::Operate && token.covers(&colony.org, &colony.repo)
}

/// Sends one event to every subscription that wants it and may see it, each through the outbox
/// with its own secret. Answers how many receivers took it on the first attempt.
pub(super) async fn fan_out(app: &App, client: &reqwest::Client, payload: &Value, colony: Option<&Session>) -> usize {
    let event = payload["event"].as_str().unwrap_or_default();
    let subscriptions = {
        let _guard = STORE.lock().await;
        load(app)
    };
    let Ok(body) = serde_json::to_string(payload) else { return 0 };
    let mut delivered = 0;
    for subscription in subscriptions.iter().filter(|s| s.wants(event)) {
        // A subscription stored before a secret was required is never delivered to (issue #900):
        // `outbox::attempt` would refuse it, and the failure below would promise a retry that the
        // outbox's own sweep cancels. Skipped here, like the owner's URL is skipped in
        // `post_webhook`, so nothing is queued that cannot be signed.
        if subscription.secret.is_none() {
            continue;
        }
        if let Some(token_id) = &subscription.token {
            let token = app.api_tokens.scoped(token_id).await;
            if !in_scope(token.as_ref(), colony) {
                continue;
            }
        }
        let delivery = outbox::Delivery::new(
            &subscription.target(),
            &subscription.url,
            payload,
            body.clone(),
            colony.map(|c| c.id.clone()),
        );
        match outbox::send(app, client, delivery).await {
            Ok(()) => delivered += 1,
            Err((error, _)) => eprintln!(
                "notify: the webhook subscription {} failed ({error}); it will be retried",
                subscription.id
            ),
        }
    }
    delivered
}

// ---------------------------------------------------------------------------
// The API
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewSubscription {
    url: String,
    #[serde(default)]
    events: Vec<String>,
    #[serde(default)]
    secret: Option<String>,
}

/// The events a scoped token may subscribe to: the ones about a colony. Host-level events have no
/// org or repository, so they are never in a token's scope.
fn colony_event(name: &str) -> bool {
    LIFECYCLE_EVENTS.contains(&name) || matches!(name, "attention" | "needs_rebase")
}

/// Whether an address names this machine, its network or a link-local service — refused to a
/// scoped token, so a token cannot make the mothership POST to what only the mothership can reach.
/// A literal check: a public name that resolves to a private address is not caught here.
fn private_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else { return true };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return private_ip(ip);
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || name.ends_with(".internal")
        || !name.contains('.')
}

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                // 100.64.0.0/10: carrier-grade NAT, where tailnets live.
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return private_ip(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

fn bad(message: &str) -> crate::AppError {
    client_error(StatusCode::BAD_REQUEST, message)
}

/// Checks a new subscription: the address, the events and the secret. `scoped` is the token making
/// it, when one is. The secret comes back as a plain `String`: every subscription has one (issue
/// #900), so a caller never has to hold the option.
fn validate(req: &NewSubscription, scoped: Option<&ScopedToken>) -> Result<(String, Vec<String>, String), crate::AppError> {
    let url = req.url.trim().to_string();
    if url.len() > MAX_URL || !webhook_valid(&url) {
        return Err(bad("url must be an http:// or https:// address"));
    }
    let parsed = reqwest::Url::parse(&url).map_err(|_| bad("url must be an http:// or https:// address"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(bad("put credentials in the secret, not in the url"));
    }
    if scoped.is_some() && private_host(&parsed) {
        return Err(bad(
            "a scoped token's webhook must be a public address, not this machine or its network",
        ));
    }
    let mut events: Vec<String> = Vec::new();
    for name in &req.events {
        let name = name.trim();
        if !EVENT_NAMES.contains(&name) {
            return Err(bad(&format!("unknown event `{name}`; see docs/protocol/webhooks.md")));
        }
        if scoped.is_some() && !colony_event(name) {
            return Err(bad(&format!(
                "`{name}` is not about a colony, so it is outside every scoped token's scope"
            )));
        }
        if !events.iter().any(|e| e == name) {
            events.push(name.to_string());
        }
    }
    let secret = match req.secret.as_deref().map(str::trim) {
        // A subscription with no secret is one this module would have to deliver to unsigned, and
        // it does not do that (issue #900), so it is refused here rather than accepted and then
        // dropped on every attempt.
        None | Some("") => {
            return Err(bad(
                "every webhook subscription needs a signing secret: a delivery is never sent unsigned",
            ));
        }
        Some(value) if value.len() > 512 || !value.chars().all(|c| c.is_ascii_graphic()) => {
            return Err(bad("that doesn't look like a signing secret"));
        }
        Some(value) => value.to_string(),
    };
    Ok((url, events, secret))
}

/// `POST /api/webhooks`: registers one subscription for the caller — the owner, or the scoped
/// token presenting it.
pub async fn create(
    State(app): State<Shared>,
    scoped: Option<Extension<ScopedToken>>,
    Json(req): Json<NewSubscription>,
) -> ApiResult<Value> {
    let scoped = scoped.map(|Extension(token)| token);
    let (url, events, secret) = validate(&req, scoped.as_ref())?;
    let _guard = STORE.lock().await;
    let mut list = load(&app);
    let token = scoped.as_ref().map(|t| t.id.clone());
    if list.len() >= MAX_TOTAL || (token.is_some() && list.iter().filter(|s| s.token == token).count() >= MAX_PER_TOKEN) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "too many webhook subscriptions; delete one first",
        ));
    }
    let subscription = Subscription {
        id: format!("whs_{}", short_id()),
        url,
        events,
        secret: Some(secret),
        token,
        created_at: Utc::now(),
    };
    list.push(subscription.clone());
    save(&app, &list)?;
    Ok(Json(subscription.view()))
}

/// `GET /api/webhooks`: the owner's view is every subscription; a scoped token's is its own.
pub async fn list(State(app): State<Shared>, scoped: Option<Extension<ScopedToken>>) -> Json<Vec<Value>> {
    let mine = scoped.map(|Extension(token)| token.id);
    let _guard = STORE.lock().await;
    Json(
        load(&app)
            .iter()
            .filter(|s| mine.is_none() || s.token == mine)
            .map(Subscription::view)
            .collect(),
    )
}

/// `DELETE /api/webhooks/{id}`: removes one. A scoped token may remove only its own; anything else
/// is a 404, the answer an unknown id gets, so it learns nothing about other tokens' subscriptions.
pub async fn remove(
    State(app): State<Shared>,
    scoped: Option<Extension<ScopedToken>>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    let mine = scoped.map(|Extension(token)| token.id);
    let _guard = STORE.lock().await;
    let mut list = load(&app);
    let before = list.len();
    list.retain(|s| !(s.id == id && (mine.is_none() || s.token == mine)));
    if list.len() == before {
        return Err(client_error(StatusCode::NOT_FOUND, "no webhook subscription has that id"));
    }
    save(&app, &list)?;
    Ok(Json(json!({"deleted": id})))
}
