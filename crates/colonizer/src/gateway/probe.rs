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

/// The reachability half of [`probe`], and the recording half of model discovery (issue #1167):
/// every caller's fresh probe flows through here, so a real answer is recorded at the one place
/// the parsed `data` list is in hand. Only a real answer records — a parsed list, or the
/// published-nothing 404 below as an empty list; a refused (401), throttled (429) or failed (5xx)
/// status, a 200 whose body carries no list, and every unreachable probe leave the stored entry
/// alone, so a blip never wipes it.
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
            let models: Vec<String> = body["data"]
                .as_array()
                .map(|data| data.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            // An Anthropic-compatible endpoint need not serve /v1/models (Alibaba's /apps/anthropic
            // doesn't): a 404 there means reachable with no published list, not a broken provider.
            let note = (provider.wire == crate::providers::Wire::Anthropic && status == 404).then_some("no model list");
            // A save's spawned probe can still be in flight when a concurrent delete has dropped the
            // provider and its entry; recording only a provider still on file keeps that late answer
            // from re-inserting an orphan.
            if (body["data"].is_array() || note.is_some()) && app.providers().iter().any(|p| p.id == provider.id) {
                app.provider_models.record(provider, &models);
            }
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
pub(crate) async fn probe_quota(app: &App, provider: &Provider) -> Option<Value> {
    quota_probe(app, provider).await
}

async fn quota_probe(app: &App, provider: &Provider) -> Option<Value> {
    let quota = provider.quota.as_ref()?;
    let mut request = app.gateway.client.get(&quota.url).timeout(HEALTH_TIMEOUT);
    if let Some((name, value)) = credential_header(app, provider) {
        request = request.header(name, value);
    }
    let mut reset_unix: Option<i64> = None;
    let (error, remaining, limit) = match request.send().await {
        Ok(response) => {
            let status = response.status();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            if status.is_success() {
                let limit = quota.limit_pointer.as_deref().and_then(|p| quota_remaining(&body, p));
                reset_unix = quota.reset_pointer.as_deref().and_then(|p| quota_reset(&body, p));
                (None, quota_remaining(&body, &quota.pointer), limit)
            } else {
                (Some(format!("quota endpoint answered HTTP {}", status.as_u16())), None, None)
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
            (Some(error), None, None)
        }
    };
    // A success with nothing readable at the pointer is the usual typo, so it gets its own words.
    let error = error.or_else(|| remaining.is_none().then(|| format!("no number at {}", quota.pointer)));
    // Every successful read is a point on the providers page's credits chart (#1204).
    if let Some(left) = remaining.as_ref().and_then(Value::as_f64) {
        app.gateway.history.record_balance(
            &provider.id,
            left,
            limit.as_ref().and_then(Value::as_f64),
            reset_unix,
            chrono::Utc::now(),
        );
    }
    let mut answer = json!({"remaining": remaining, "error": error});
    // The plan's total only when a limit pointer is configured, so older readers see the same shape.
    if quota.limit_pointer.is_some() {
        answer["limit"] = limit.unwrap_or(Value::Null);
    }
    if quota.reset_pointer.is_some() {
        answer["reset_unix"] = json!(reset_unix);
    }
    Some(answer)
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

/// The moment a quota reset pointer names, as unix seconds: a number (milliseconds when it is too big
/// to be seconds), a numeric string, or an RFC 3339 string. `None` when the pointer misses.
pub(crate) fn quota_reset(body: &Value, pointer: &str) -> Option<i64> {
    let value = body.pointer(pointer)?;
    if let Some(text) = value.as_str()
        && let Ok(at) = chrono::DateTime::parse_from_rfc3339(text.trim())
    {
        return Some(at.timestamp());
    }
    let n = value.as_f64().or_else(|| value.as_str()?.trim().parse().ok())?;
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    Some(if n > 1e11 { (n / 1000.0) as i64 } else { n as i64 })
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

/// How long the save-time test request may take: longer than the models probe, since it asks for a
/// real (one-token) completion, but short enough that the Settings form never hangs on it.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Sends a one-token request through the same route a colony's turn takes — the anthropic wire's
/// `/v1/messages`, or the openai wire's translated `/v1/chat/completions` — joined to the base URL
/// exactly as the gateway joins it, with the provider's credential and connection policy (issue
/// #1018). The answer names the final URL (redacted: no userinfo or query) and the status, so a base
/// URL that misses the provider's API root shows up when the provider is saved, not in a hundred
/// failed colony turns. It tests with the provider's first listed model.
pub async fn test_request(app: &App, provider: &Provider) -> Value {
    let failed = |url: Option<String>, model: Option<&str>, error: String| json!({"ok": false, "url": url, "status": null, "model": model, "latency_ms": null, "error": error});
    let Some(model) = provider.models.first().map(String::as_str) else {
        return failed(None, None, "list at least one model to test with".into());
    };
    let request = json!({"model": model, "max_tokens": 1, "messages": [{"role": "user", "content": "ping"}]});
    let raw = Bytes::from(request.to_string());
    let body = apply_connection_policy(&raw, provider).map(Bytes::from).unwrap_or(raw);
    let (path, body) = match provider.wire {
        Wire::Anthropic => {
            let body = normalize_anthropic_body(&body, provider.quirks()).map_or(body, |(normalized, _)| normalized);
            ("/v1/messages", body)
        }
        Wire::Openai => match openai::translate_request(&body) {
            Ok((translated, _)) => ("/v1/chat/completions", Bytes::from(translated)),
            Err(message) => return failed(None, Some(model), message),
        },
    };
    let url = upstream_url(&provider.base_url, path, None).expect("the test path is clean");
    let shown = reqwest::Url::parse(&url).map(|u| redacted_url(&u)).ok();
    let mut request = app
        .gateway
        .client
        .post(&url)
        .timeout(TEST_TIMEOUT)
        .header("content-type", "application/json")
        .body(body);
    if provider.wire == Wire::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }
    if let Some((name, value)) = credential_header(app, provider) {
        request = request.header(name, value);
    }
    let started = Instant::now();
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            let shown = redacted_url(response.url());
            let latency_ms = started.elapsed().as_millis() as u64;
            let bytes = response.bytes().await.unwrap_or_default();
            let error = (!status.is_success()).then(|| {
                let parsed: Value = serde_json::from_slice(&bytes).unwrap_or_default();
                let detail = parsed["error"]["message"]
                    .as_str()
                    .or_else(|| parsed["message"].as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
                let detail: String = detail.chars().take(300).collect();
                match wrong_route_hint(status.as_u16(), &shown) {
                    Some(hint) => format!("{detail} ({hint})"),
                    None => detail,
                }
            });
            json!({
                "ok": status.is_success(),
                "url": shown,
                "status": status.as_u16(),
                "model": model,
                "latency_ms": latency_ms,
                "error": error,
            })
        }
        Err(e) => {
            let error = if e.is_timeout() {
                format!("no response within {} s", TEST_TIMEOUT.as_secs())
            } else if e.is_connect() {
                "connection failed".to_string()
            } else {
                e.without_url().to_string()
            };
            failed(shown, Some(model), error)
        }
    }
}

/// `POST /api/providers/{id}/test`: [`test_request`] against a saved provider.
pub async fn provider_test(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<Value> {
    let provider = app
        .providers()
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such provider"))?;
    Ok(Json(test_request(&app, &provider).await))
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    tokio::spawn(flush_loop(app.clone()));
    // The daily model-list sweep (issue #1167) keeps its own cadence; see `discover::refresh_loop`.
    tokio::spawn(discover::refresh_loop(app.clone()));
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/providers/{id}/health", routing::get(provider_health))
        .route("/api/providers/{id}/test", routing::post(provider_test))
}
