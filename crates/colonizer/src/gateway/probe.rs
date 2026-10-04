use super::*;

/// Cache key for a probe result: the provider id plus its endpoint, so repointing a provider at
/// a new URL never serves the old endpoint's answer.
pub fn probe_cache_key(provider: &Provider) -> String {
    format!("{}\n{}", provider.id, provider.base_url)
}

/// The boot-time probe with a short read-through cache ([`PROVIDER_PROBE_TTL`]). Both reachable
/// and unreachable answers are cached, and the boot still logs its unreachable warning on every
/// boot, from the cached value when that is what was used. The one call the cache does not make is
/// the boot refusal: with no `fallback_model` the boot re-probes fresh before refusing, so an
/// endpoint that came back within the TTL is not refused on a stale unreachable answer. The lock is
/// not held across the probe, so one slow (or dead) provider does not hold up the others sharing
/// this map; concurrent boots racing a cold entry may each probe once, which the TTL then coalesces.
pub async fn probe_cached(app: &App, provider: &Provider) -> Value {
    let key = probe_cache_key(provider);
    {
        let cache = app.provider_probe_cache.lock().await;
        if let Some((probed_at, value)) = cache.get(&key)
            && probed_at.elapsed() < PROVIDER_PROBE_TTL
        {
            return value.clone();
        }
    }
    let health = probe(app, provider).await;
    store_probe(app, provider, &health).await;
    health
}

/// Records a probe result under [`probe_cache_key`], stamped now.
async fn store_probe(app: &App, provider: &Provider, health: &Value) {
    app.provider_probe_cache
        .lock()
        .await
        .insert(probe_cache_key(provider), (Instant::now(), health.clone()));
}

/// Drops every cached probe for a provider id, whatever endpoint it was probed at. The cache key
/// carries the base URL but not the credential, so rotating a key or changing the auth mode would
/// otherwise keep serving the old answer for up to [`PROVIDER_PROBE_TTL`]. Callers mutate providers
/// in providers.rs (`put`, `delete`); the health handler needs no call, it always probes fresh.
pub async fn forget_probe(app: &App, id: &str) {
    let prefix = format!("{id}\n");
    app.provider_probe_cache
        .lock()
        .await
        .retain(|key, _| !key.starts_with(&prefix));
}

/// Probes `GET {base_url}/v1/models` with the provider's credential — plus, when the provider has a
/// quota probe ([`Provider.quota`]), the quota URL, sent at the same time so one slow endpoint does
/// not stretch the check. The quota answer rides along under `quota` and never changes the
/// reachability verdict: reading a plan balance is not a health check (issue #199).
pub async fn probe(app: &App, provider: &Provider) -> Value {
    let (mut health, quota) = tokio::join!(models_probe(app, provider), quota_probe(app, provider));
    if let Some(quota) = quota {
        health["quota"] = quota;
    }
    health
}

/// The reachability half of [`probe`].
async fn models_probe(app: &App, provider: &Provider) -> Value {
    let started = Instant::now();
    let mut request = app
        .gateway
        .client
        .get(upstream_url(&provider.base_url, "/v1/models", None).expect("the probe path is clean"))
        .timeout(HEALTH_TIMEOUT);
    if let Some((name, value)) = credential_header(app, provider) {
        request = request.header(name, value);
    }
    let checked_at = chrono::Utc::now();
    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            let models: Vec<Value> = body["data"]
                .as_array()
                .map(|data| data.iter().filter_map(|m| m["id"].as_str()).map(|id| json!(id)).collect())
                .unwrap_or_default();
            // An Anthropic-compatible endpoint need not serve /v1/models (Alibaba's /apps/anthropic
            // doesn't): a 404 there means reachable with no published list, not a broken provider.
            let note = (provider.wire == crate::providers::Wire::Anthropic && status == 404).then_some("no model list");
            json!({
                "reachable": true,
                "status": status,
                "latency_ms": started.elapsed().as_millis() as u64,
                "models": models,
                "error": null,
                "note": note,
                "checked_at": checked_at,
            })
        }
        Err(e) => {
            let error = if e.is_timeout() {
                format!("no response within {} s", HEALTH_TIMEOUT.as_secs())
            } else if e.is_connect() {
                "connection failed".to_string()
            } else {
                e.without_url().to_string()
            };
            json!({"reachable": false, "status": null, "latency_ms": null, "models": [], "error": error, "note": null, "checked_at": checked_at})
        }
    }
}

/// The quota half of [`probe`]: `GET`s the configured URL with the same credential and timeout, and
/// reads the pointer out of the answer. `None` when the provider has no probe, which leaves the
/// `quota` field out of the health JSON entirely.
async fn quota_probe(app: &App, provider: &Provider) -> Option<Value> {
    let quota = provider.quota.as_ref()?;
    let mut request = app.gateway.client.get(&quota.url).timeout(HEALTH_TIMEOUT);
    if let Some((name, value)) = credential_header(app, provider) {
        request = request.header(name, value);
    }
    let (error, remaining) = match request.send().await {
        Ok(response) => {
            let status = response.status();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            if status.is_success() {
                (None, quota_remaining(&body, &quota.pointer))
            } else {
                (Some(format!("quota endpoint answered HTTP {}", status.as_u16())), None)
            }
        }
        Err(e) => {
            let error = if e.is_timeout() {
                format!("no response within {} s", HEALTH_TIMEOUT.as_secs())
            } else if e.is_connect() {
                "connection failed".to_string()
            } else {
                e.without_url().to_string()
            };
            (Some(error), None)
        }
    };
    // A success with nothing readable at the pointer is the usual typo, so it gets its own words.
    let error = error.or_else(|| remaining.is_none().then(|| format!("no number at {}", quota.pointer)));
    Some(json!({"remaining": remaining, "error": error}))
}

/// The number a quota pointer points at: a JSON number, or a numeric string — some plans quote the
/// count. `None` when the pointer misses or lands on anything else, which the caller reports as an
/// error rather than as a balance.
pub(super) fn quota_remaining(body: &Value, pointer: &str) -> Option<Value> {
    let value = body.pointer(pointer)?;
    let count = value.as_f64().or_else(|| value.as_str()?.trim().parse().ok())?;
    // A plan's remaining is a whole count, so it stays one in JSON: `12000.0` would read like an
    // estimate instead of a balance.
    Some(if count.fract() == 0.0 {
        json!(count as i64)
    } else {
        json!(count)
    })
}

pub async fn provider_health(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let provider = app
        .providers()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such provider"))?;
    // Always a fresh probe, but written through to the cache so the next boot reuses it.
    let health = probe(&app, &provider).await;
    store_probe(&app, &provider, &health).await;
    Ok(Json(health))
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    tokio::spawn(flush_loop(app.clone()));
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/providers/{id}/health", routing::get(provider_health))
}
