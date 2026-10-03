use super::*;

/// Attention reason set when a colony's last turn failed on the model/provider side (upstream 4xx/5xx).
/// Owned by the gateway — the watchdog owns `WATCHDOG_REASONS`, autopilot owns its hold — and shared
/// with usage.rs, which buckets it as a closed failure label. This is the hook point the proxy calls;
/// fuller session wiring (clearing rules and the like) stays with its owner (#230).
pub(crate) const MODEL_ERROR_REASON: &str = "model_error";

/// Flags a colony for attention after a model/provider error, leaving an existing flag alone: the
/// watchdog's or autopilot's reason describes the colony better than a fresh error does.
pub(crate) async fn flag_model_error(app: &Shared, colony: &str) {
    app.update_session(colony, |x| {
        if x.attention.is_none() {
            x.attention = Some(json!({"reason": MODEL_ERROR_REASON, "since": Utc::now()}));
        }
    })
    .await;
}

/// Lifts a `model_error` flag once the colony's provider answers again: the error the flag named is
/// over, so it no longer needs anyone. Any other reason (the watchdog's, autopilot's) is left alone.
/// Without this a colony that recovered from one failed call stayed on the needs-you list for good,
/// and one whose calls alternate between failing and succeeding flickered on and off it.
pub(crate) async fn clear_model_error(app: &Shared, colony: &str) {
    // Every success path lands here, so it is also where a colony's quota block lifts (#760).
    app.gateway.clear_colony_quota(colony);
    let flagged = app
        .session(colony)
        .await
        .and_then(|s| s.attention)
        .is_some_and(|a| a.get("reason").and_then(serde_json::Value::as_str) == Some(MODEL_ERROR_REASON));
    if flagged {
        app.update_session(colony, |x| {
            if x.attention
                .as_ref()
                .and_then(|a| a.get("reason"))
                .and_then(serde_json::Value::as_str)
                == Some(MODEL_ERROR_REASON)
            {
                x.attention = None;
            }
        })
        .await;
    }
}

/// Rewrites an Anthropic-wire request body for a provider with dialect quirks
/// ([`Provider::quirks`]). Returns `None` when the body needs nothing changed, so the caller keeps
/// the original bytes and the passthrough stays byte-identical for providers without quirks. A body
/// that is not JSON is also left alone: only real requests are rewritten, never anything else.
/// Currently: strips `ttl` from every `cache_control` object — system blocks, message content blocks,
/// tools — via a whole-body walk, so no location is special-cased — and raises `max_tokens` below the
/// provider's floor. Returns the rewritten bytes plus a short human summary of what changed.
pub fn normalize_anthropic_body(body: &Bytes, quirks: ProviderQuirks) -> Option<(Bytes, String)> {
    if !quirks.needs_normalize() {
        return None;
    }
    let mut value: Value = serde_json::from_slice(body).ok()?;
    let mut stripped = 0u64;
    if quirks.strip_cache_ttl {
        strip_cache_ttl(&mut value, &mut stripped);
    }
    let mut raised = false;
    if let Some(min) = quirks.min_max_tokens
        && let Some(max) = value.get("max_tokens").and_then(Value::as_u64)
        && max < min
    {
        value["max_tokens"] = json!(min);
        raised = true;
    }
    if stripped == 0 && !raised {
        return None;
    }
    let mut notes = Vec::new();
    if stripped > 0 {
        notes.push(format!("stripped cache_control.ttl from {stripped} block(s)"));
    }
    if raised {
        notes.push(format!("raised max_tokens to {}", quirks.min_max_tokens.unwrap_or_default()));
    }
    let out = serde_json::to_vec(&value).ok()?;
    Some((Bytes::from(out), notes.join("; ")))
}

/// Drops `ttl` from every `{"type":"ephemeral",…}` cache_control object in the value, counting removals.
fn strip_cache_ttl(value: &mut Value, stripped: &mut u64) {
    match value {
        Value::Object(map) => {
            if let Some(cc) = map.get_mut("cache_control")
                && cc.get("type").and_then(Value::as_str) == Some("ephemeral")
                && cc.get("ttl").is_some()
                && let Some(cc) = cc.as_object_mut()
            {
                cc.remove("ttl");
                *stripped += 1;
            }
            for value in map.values_mut() {
                strip_cache_ttl(value, stripped);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_cache_ttl(item, stripped);
            }
        }
        _ => {}
    }
}

/// The output side assumed for a request that names no cap of its own (or whose body is not JSON).
pub(super) const ESTIMATED_MAX_TOKENS: u64 = 4096;

/// Dollars as micro-dollars, the fixed-point form the reservation counter keeps: floats have no
/// stable-Rust atomics, and a sixth of a cent is finer than any estimate here claims to be.
pub(super) fn micro_usd(usd: f64) -> u64 {
    (usd * 1_000_000.0).round().max(0.0) as u64
}

/// What one request is estimated to spend, so its cost can be reserved before dispatch. Intentionally
/// approximate (issue #409): the estimate bounds how far a burst of parallel requests can overshoot
/// the budget before any of them records its real cost, and is never billed. The output side reads
/// the request's own `max_tokens` — `max_completion_tokens` in Chat Completions' spelling,
/// `max_output_tokens` in Responses' — falling back to [`ESTIMATED_MAX_TOKENS`] when it names neither
/// or is not JSON; the input side is the body's bytes over four, the usual tokens-per-byte rule of
/// thumb. A provider without pricing estimates at $0, exactly what recording it would cost. This
/// being the body's one parse, the requested model rides along for the audit record — which only ever
/// names a model the model-id validator passed, never raw body text.
pub(super) fn estimate_request_cost_usd(provider: &Provider, body: &Bytes) -> (f64, Option<String>) {
    let request: Value = serde_json::from_slice(body).unwrap_or_default();
    let output_tokens = request
        .get("max_tokens")
        .or_else(|| request.get("max_completion_tokens"))
        .or_else(|| request.get("max_output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(ESTIMATED_MAX_TOKENS);
    let model = request["model"]
        .as_str()
        .and_then(|m| valid_model(m).then_some(m.to_string()));
    (
        provider.cost_usd(Usage {
            input_tokens: body.len() as u64 / 4,
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            thinking_tokens: 0,
        }),
        model,
    )
}

/// A colony token from `Authorization: Bearer <token>`: the same secret, in the form an OpenAI-wire
/// runner sends by default. Not the scheme Claude Code uses, but the comparison is the same
/// constant-time one [`Gateway::colony_for_token`] applies to either form.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix(BEARER_PREFIX)
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// The request body with its `model` replaced, for the fallback retry; `None` for a body that is
/// not a JSON object.
fn with_model(body: &Bytes, model: &str) -> Option<Bytes> {
    let mut request: Value = serde_json::from_slice(body).ok()?;
    request.as_object_mut()?.insert("model".into(), json!(model));
    serde_json::to_vec(&request).ok().map(Bytes::from)
}

/// The gateway's route for `/providers/{id}/...`: one pass through [`proxy_to`], plus — when the
/// provider answered quota-exhausted and its `fallback_model` is `<provider>/<model>` on another
/// same-wire provider — one retry there with the model swapped (issue #767). A Claude fallback stays
/// the colony router's; the retry is a single hop, and a fallback provider that is out of quota
/// itself, missing, or on another wire leaves the original answer (and the colony blocked on it).
pub(super) async fn proxy(
    State(app): State<Shared>,
    Path((id, _)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let response = proxy_to(
        app.clone(),
        id.clone(),
        method.clone(),
        uri.clone(),
        headers.clone(),
        body.clone(),
        false,
    )
    .await;
    if !response.headers().contains_key(QUOTA_HEADER) || !Gateway::quota_fallback_enabled() {
        return response;
    }
    let providers = app.providers();
    let Some(provider) = providers.iter().find(|p| p.id == id) else {
        return response;
    };
    let Some((to, model)) = provider.provider_fallback() else {
        return response;
    };
    let (to, model) = (to.to_string(), model.to_string());
    // The colony token arrives as `x-colonizer-colony` — the header every runner is configured with —
    // or as a plain `Authorization: Bearer` for one that speaks ordinary HTTP auth (issue #629). The
    // gateway's own header keeps precedence, and neither credential is ever forwarded upstream: the
    // outgoing headers are built from scratch below, carrying only the provider's own key.
    let token = headers
        .get(COLONY_HEADER)
        .and_then(|v| v.to_str().ok())
        .or_else(|| bearer_token(&headers))
        .unwrap_or_default();
    let colony = app.colony_for_token(token).await.map(|s| s.id);
    let skip = match providers.iter().find(|p| p.id == to) {
        None => Some("is not configured"),
        Some(target) if target.wire != provider.wire => Some("speaks another wire"),
        Some(_) if app.gateway.is_quota_exhausted(&to) => Some("is out of quota too"),
        Some(_) => None,
    };
    let rest = uri.path().strip_prefix(&format!("/providers/{id}")).unwrap_or_default();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let retry = match (
        skip,
        with_model(&body, &model),
        format!("/providers/{to}{rest}{query}").parse::<Uri>(),
    ) {
        (None, Some(body), Ok(uri)) => Some((body, uri)),
        _ => None,
    };
    let Some((body, retry_uri)) = retry else {
        eprintln!(
            "gateway: provider \"{id}\" is out of quota and its fallback {to}/{model} {}; no retry",
            skip.unwrap_or("could not take the request")
        );
        // No retry is coming after all, so the colony is blocked on this provider (#760, #767).
        if let Some(colony) = &colony {
            app.gateway.note_colony_quota(colony, &id);
        }
        return response;
    };
    drop(response);
    eprintln!("gateway: provider \"{id}\" is out of quota; retrying on its fallback {to}/{model}");
    proxy_to(app, to, method, retry_uri, headers, body, true).await
}

/// One pass of a colony request to provider `id`. `fallback` marks the gateway's own quota retry
/// ([`proxy`]): the operator configured that hop on the provider, so the colony's recorded
/// provider and model scope (which admitted the first pass) does not refuse it; every other check —
/// sensitivity, key, budget — applies as to any request.
async fn proxy_to(
    app: Shared,
    id: String,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    fallback: bool,
) -> Response {
    // The colony token arrives as `x-colonizer-colony` — the header every runner is configured with —
    // or as a plain `Authorization: Bearer` for one that speaks ordinary HTTP auth (issue #629). The
    // gateway's own header keeps precedence, and neither credential is ever forwarded upstream: the
    // outgoing headers are built from scratch below, carrying only the provider's own key.
    let token = headers
        .get(COLONY_HEADER)
        .and_then(|v| v.to_str().ok())
        .or_else(|| bearer_token(&headers))
        .unwrap_or_default();
    let Some(session) = app.colony_for_token(token).await else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "colonizer gateway: unknown colony",
            None,
        );
    };
    let colony = session.id.clone();
    // The request's one audit record starts here — past the token check, so an unknown token is
    // the only request that leaves no line: it cannot be attributed to a colony. `rest` is the
    // provider-relative path; the query string never reaches the record. The requested model lands
    // later, from the body's one parse, in `estimate_request_cost_usd`.
    let rest = uri.path().strip_prefix(&format!("/providers/{id}")).unwrap_or_default();
    let audit = GatewayAudit::start(
        crate::gateway_audit::log_path(&app, &colony),
        &colony,
        &id,
        method.as_str(),
        rest,
        body.len(),
    );
    let Some(provider) = app.providers().into_iter().find(|p| p.id == id) else {
        audit.fail(StatusCode::NOT_FOUND.as_u16(), GatewayFailure::UnknownProvider);
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("colonizer gateway: no provider \"{id}\""),
            None,
        );
    };
    audit.set_wire(crate::gateway_audit::wire_name(provider.wire));
    // A configured provider is not necessarily this colony's (issue #409): providers.json is
    // mothership-wide, so the token alone must not open one the colony's model settings never
    // routed to. A colony with no recorded set reaches no provider either (issue #681): the set is
    // what the token's access is derived from, and boot records it before the token is written.
    // Refused with the other local refusals — before credentials, budget, or any upstream call —
    // and like the budget 403, one Claude Code does not retry in a loop.
    if !fallback
        && !session
            .allowed_providers
            .as_ref()
            .is_some_and(|allowed| allowed.contains(&id))
    {
        audit.fail(StatusCode::FORBIDDEN.as_u16(), GatewayFailure::NotRouted);
        return api_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!(
                "colonizer gateway: provider \"{id}\" is not routed to colony {colony}; a colony spends only on the providers its model settings route to"
            ),
            None,
        );
    }
    // Only the request shape the wires serve goes any further (issue #681): Anthropic Messages on
    // either wire — plus count_tokens where the provider serves it natively — and, on the openai
    // wire, the provider's own Responses and Chat Completions routes that a runner speaking that
    // wire itself posts (codex, grok-build; issue #629). Anything else is refused before the
    // provider's credential is ever attached, and the refusal keeps the audited failure code.
    let served = rest == "/v1/messages"
        || (matches!(provider.wire, Wire::Anthropic) && rest == "/v1/messages/count_tokens")
        || (matches!(provider.wire, Wire::Openai) && is_openai_passthrough(rest));
    if !served {
        audit.fail(StatusCode::NOT_FOUND.as_u16(), GatewayFailure::BadRequest);
        return api_error(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("colonizer gateway: provider \"{id}\" does not serve {rest}"),
            None,
        );
    }
    if method != Method::POST {
        audit.fail(StatusCode::METHOD_NOT_ALLOWED.as_u16(), GatewayFailure::BadRequest);
        return api_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_request_error",
            format!("colonizer gateway: provider \"{id}\" serves {rest} with POST, not {method}"),
            None,
        );
    }
    // A task whose issue named sensitive paths may only reach a provider whose mark meets the
    // class's minimum — vetted work needs a vetted provider, restricted work a trusted one, the
    // looser classes any provider unless this org's sensitivity overrides move the bar, and
    // restricted work can be pinned to vendors outright (sensitivity.rs, issue #472). Checked
    // independently of allowed_providers above, which is about which providers this colony's
    // *models* route to, not which ones its *task* may trust with sensitive content. The policy
    // itself lives in `eligible`; the gateway only reads the class back off the session record.
    if let Some(sensitivity) = session
        .sensitivity
        .as_deref()
        .and_then(crate::sensitivity::Sensitivity::parse)
    {
        let overrides = app.org_settings(&session.org).sensitivity;
        let overrides = overrides.as_ref();
        let mark = crate::sensitivity::ProviderMark::of(provider.trusted, provider.vetted);
        if !crate::sensitivity::eligible(sensitivity, mark, provider.vendor.as_deref(), overrides) {
            // The mark can be the blocker, or — when the org pins vendors — the vendor can be even
            // though the mark already meets the bar; name the one that actually refused it.
            let required = crate::sensitivity::required_mark(sensitivity, overrides);
            let fix = if mark < required {
                format!("mark it {} in providers.json to allow it", required.as_str())
            } else {
                "its vendor is not on this org's restricted-vendor list".to_string()
            };
            audit.fail(StatusCode::FORBIDDEN.as_u16(), GatewayFailure::Restricted);
            app.session_log(
                &colony,
                "warn",
                format!(
                    "colonizer gateway: refused provider \"{id}\" — this colony's task touches {} paths and \"{id}\" does not meet the bar ({fix})",
                    sensitivity.as_str()
                ),
            )
            .await;
            return api_error(
                StatusCode::FORBIDDEN,
                "sensitivity_error",
                format!(
                    "colonizer gateway: provider \"{id}\" is not eligible for colony {colony}'s {}-sensitivity task; {fix}",
                    sensitivity.as_str()
                ),
                None,
            );
        }
    }
    // A keyed provider with no saved key would otherwise be sent the request with no credential at all,
    // and answer with a bare 401 that says nothing about why. Refused here instead, before any upstream
    // call, naming the provider and where the key goes.
    if provider.auth != "none" && credential_header(&app, &provider).is_none() {
        audit.fail(StatusCode::BAD_GATEWAY.as_u16(), GatewayFailure::MissingKey);
        return api_error(
            StatusCode::BAD_GATEWAY,
            "api_error",
            format!(
                "colonizer gateway: provider \"{id}\" needs an API key and none is saved; add it in Settings → Model providers"
            ),
            None,
        );
    }
    // Refused before it waits for a slot, and the colony is stopped like the max-duration path stops one.
    // The 403 follows the empty-balance precedent in openai.rs: Claude Code does not retry it in a loop.
    if crate::lifecycle::enforce_budget(&app, &colony).await {
        audit.fail(StatusCode::FORBIDDEN.as_u16(), GatewayFailure::Budget);
        return api_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!("colonizer gateway: colony {colony} passed its budget and was stopped; raise the budget and resume it"),
            None,
        );
    }
    // The budget check above has a blind spot (issue #409): it compares spend already recorded, and a
    // gateway request's cost is only recorded once its response has finished streaming, so parallel
    // requests each pass before any of them lands. Reserve an estimate of this one's cost for as long
    // as it is in flight; when recorded spend plus outstanding reservations plus this estimate would
    // tip a currently healthy colony past the cap, refuse this one request — stopping the colony stays
    // `enforce_budget`'s job, for overspend that has actually been recorded.
    let modules = app.modules.read().await.clone();
    let budget = orgs::budget_usd(&modules, &app.org_settings(&session.org));
    // The body's one parse, shared by the estimate and the audit record's requested model.
    let (estimate, model) = estimate_request_cost_usd(&provider, &body);
    audit.set_model(model.clone());
    // A model the colony's settings never routed to is refused like a provider outside them
    // (issue #681): the body's model is what the provider is asked to serve, and the token alone
    // does not open the rest of a configured provider's catalogue. Checked on the requested name —
    // before any `model_map` renaming — and before the spend reservation, so a refused request
    // reserves nothing. Guests send the bare canonical name (the runner strips the `<provider>/`
    // prefix), which is the form boot recorded.
    // A colony booted before model scoping (#727) has a recorded provider set but no model set.
    // Refusing it every model stranded running colonies at the upgrade (every subagent 403'd), so
    // such a colony keeps the pre-#727 scope, its recorded providers, until it next boots and
    // records its models. A colony with neither set is still refused at the provider check above.
    let legacy = session.allowed_models.is_none() && session.allowed_providers.is_some();
    let routed = fallback
        || legacy
        || model
            .as_deref()
            .map(|m| format!("{id}/{m}"))
            .is_some_and(|pair| session.allowed_models.as_ref().is_some_and(|allowed| allowed.contains(&pair)));
    if !routed {
        audit.fail(StatusCode::FORBIDDEN.as_u16(), GatewayFailure::NotRouted);
        let named = model.unwrap_or_else(|| "none".into());
        return api_error(
            StatusCode::FORBIDDEN,
            "permission_error",
            format!(
                "colonizer gateway: model \"{named}\" is not routed to colony {colony}; a colony spends only on the models its agent settings name"
            ),
            None,
        );
    }
    let reserved = app.gateway.colony_reserved(&colony);
    // Recorded spend and the reservation are read and claimed together, under the sessions read
    // lock: a recorder hands its estimate back under the write lock in the same step that adds the
    // real cost (`record_routed_usage`), so this check never sees a cost twice or not at all, and the
    // compare-and-swap in `try_new` keeps parallel requests from both passing on one total.
    let reservation = {
        let sessions = app.sessions.read().await;
        let recorded = sessions
            .iter()
            .find(|s| s.id == colony)
            .map_or(session.total_cost_usd(), |s| s.total_cost_usd());
        let fits = |outstanding: u64| budget <= 0.0 || recorded + outstanding as f64 / 1_000_000.0 + estimate <= budget;
        match Reserved::try_new(&reserved, micro_usd(estimate), fits) {
            Ok(reservation) => reservation,
            Err(outstanding) => {
                audit.fail(StatusCode::FORBIDDEN.as_u16(), GatewayFailure::Budget);
                return api_error(
                    StatusCode::FORBIDDEN,
                    "permission_error",
                    format!(
                        "colonizer gateway: colony {colony} has ${recorded:.2} recorded plus ${:.2} estimated in flight, and this request's estimated ${estimate:.2} would pass its spend budget of ${budget:.2}; wait for the in-flight requests to finish or raise the budget",
                        outstanding as f64 / 1_000_000.0
                    ),
                    None,
                );
            }
        }
    };
    // Everything that can refuse the request happens here, before it waits for a slot. The anthropic
    // wire never parses the body; the openai wire either passes its own routes through (rewriting only
    // the model) or rebuilds an Anthropic body into Chat Completions. Either wire's refusal of a body
    // it cannot forward reads the same.
    let bad_request = |message: String| -> Response {
        audit.fail(StatusCode::BAD_REQUEST.as_u16(), GatewayFailure::BadRequest);
        api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("colonizer gateway: {message}"),
            None,
        )
    };
    let (url, upstream_headers, body, handling) = match provider.wire {
        Wire::Anthropic => {
            let Some(url) = upstream_url(&provider.base_url, rest, uri.query()) else {
                return bad_request("unsupported path".into());
            };
            // Preemptive, not a retry: the provider's quirks say which fields its dialect rejects, so a
            // request carrying them is rewritten once, up front, and the rewrite is logged with the field
            // named. Anything without quirks skips this entirely and stays byte-identical. The connection
            // policy (model_map, disabled tools) runs first, for the same reason and with the same
            // byte-identical escape hatch (#295).
            let mut body = body;
            let mut wire_model = model;
            if let Some(policy) = apply_connection_policy(&body, &provider) {
                // A `model_map` entry renames the model on the wire, so the audit record's wire model
                // is read back from the rewritten body — validator applied, like the requested one.
                wire_model = serde_json::from_slice::<Value>(&policy)
                    .ok()
                    .and_then(|v| v["model"].as_str().filter(|m| valid_model(m)).map(str::to_string));
                body = Bytes::from(policy);
            }
            if let Some((normalized, note)) = normalize_anthropic_body(&body, provider.quirks()) {
                eprintln!("gateway: provider \"{id}\": normalized request body preemptively ({note})");
                body = normalized;
            }
            // Normalization rewrites fields, never the model: what goes out is the requested model,
            // or the connection policy's mapped name for it.
            audit.set_wire_model(wire_model);
            (
                url,
                forward_headers(&headers, credential_header(&app, &provider)),
                body,
                Routed::Anthropic,
            )
        }
        // A runner speaking the OpenAI wire itself (issue #629) posts one of the provider's own routes
        // and gets it forwarded as sent — only the model_map rename and, for a streaming chat
        // completion, the usage request are applied.
        Wire::Openai if is_openai_passthrough(rest) => {
            let Some(url) = upstream_url(&provider.base_url, rest, uri.query()) else {
                return bad_request("unsupported path".into());
            };
            let body = match openai_passthrough_body(rest, &body, &provider) {
                Ok(body) => Bytes::from(body),
                Err(message) => return bad_request(message),
            };
            // The audit's wire model reads back from the rewritten body — validator applied, like the
            // translated route's.
            let wire_model = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v["model"].as_str().filter(|m| valid_model(m)).map(str::to_string));
            audit.set_wire_model(wire_model);
            let mut upstream_headers = HeaderMap::new();
            upstream_headers.insert("content-type", HeaderValue::from_static("application/json"));
            if let Some((name, value)) = credential_header(&app, &provider) {
                upstream_headers.insert(name, value);
            }
            (url, upstream_headers, body, Routed::Openai)
        }
        Wire::Openai => {
            // 404 is also what tells the colony router to estimate `count_tokens` itself.
            let Some(path) = openai::upstream_path(rest) else {
                audit.fail(StatusCode::NOT_FOUND.as_u16(), GatewayFailure::BadRequest);
                return api_error(
                    StatusCode::NOT_FOUND,
                    "not_found_error",
                    format!("colonizer gateway: provider \"{id}\" does not serve {rest}"),
                    None,
                );
            };
            // The connection policy applies to the Anthropic-shaped body the runner sent, before the
            // translator reads `model` and `tools` out of it (#295).
            let body = apply_connection_policy(&body, &provider).map(Bytes::from).unwrap_or(body);
            let (body, info) = match openai::translate_request(&body) {
                Ok(translated) => translated,
                Err(message) => return bad_request(message),
            };
            audit.set_wire_model(valid_model(&info.model).then(|| info.model.clone()));
            let Some(url) = upstream_url(&provider.base_url, path, None) else {
                return bad_request("unsupported path".into());
            };
            let mut upstream_headers = HeaderMap::new();
            upstream_headers.insert("content-type", HeaderValue::from_static("application/json"));
            if let Some((name, value)) = credential_header(&app, &provider) {
                upstream_headers.insert(name, value);
            }
            (url, upstream_headers, Bytes::from(body), Routed::Translated(info))
        }
    };

    // Counted as usage from here on: everything that can refuse the request locally has passed, so every
    // remaining outcome is a real provider one (or waiting on it). A request that never gets a slot still
    // counts as a request and a failure, but the timer only starts once it is dispatched, below.
    let usage = app.gateway.usage_counters(&id);
    usage.add_request();
    let busy = Counted::new(&app.gateway.colony_counter(&colony));
    let stats = app.gateway.stats(&id);
    let timeout = Duration::from_secs(provider.timeout_secs());
    let permit: Option<OwnedSemaphorePermit> = match app.gateway.slots(&id, provider.max_concurrent) {
        None => None,
        Some(slots) => {
            let queue_timeout = provider.queue_timeout_secs();
            // A colony's own waiters are capped before it joins the queue: past the cap the
            // refusal is immediate, so one colony cannot park unbounded requests on a busy
            // provider (issue #681).
            let colony_waiting = counted_within(&app.gateway.colony_queue_counter(&colony), COLONY_QUEUE_CAP);
            if colony_waiting.is_none() {
                usage.add_failure(GatewayFailure::QueueFull);
                audit.fail(StatusCode::TOO_MANY_REQUESTS.as_u16(), GatewayFailure::QueueFull);
                return api_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "overloaded_error",
                    format!(
                        "provider \"{id}\" is busy: colony {colony} already has {COLONY_QUEUE_CAP} requests waiting for a slot"
                    ),
                    None,
                );
            }
            let waiting = Counted::new(&stats.queued);
            let queued_at = Instant::now();
            let acquired = tokio::time::timeout(Duration::from_secs(queue_timeout), slots.acquire_owned()).await;
            drop(waiting);
            drop(colony_waiting);
            match acquired {
                Ok(Ok(permit)) => {
                    audit.set_queue_ms(queued_at.elapsed().as_millis() as u64);
                    // The wait can outlast the admission: the colony may have stopped, or its
                    // token stopped matching, while this request sat in the queue. Re-read the
                    // token, and the budget the wait gave it time to pass, before anything
                    // leaves for the provider (issue #681). The spend reservation rides the
                    // return, so a refused request releases its estimate.
                    if app.colony_for_token(token).await.as_ref().map(|s| s.id.as_str()) != Some(colony.as_str()) {
                        usage.add_failure(GatewayFailure::ColonyInactive);
                        audit.fail(StatusCode::FORBIDDEN.as_u16(), GatewayFailure::ColonyInactive);
                        return api_error(
                            StatusCode::FORBIDDEN,
                            "permission_error",
                            format!("colonizer gateway: colony {colony} is no longer active; the queued request was not sent"),
                            None,
                        );
                    }
                    if crate::lifecycle::enforce_budget(&app, &colony).await {
                        usage.add_failure(GatewayFailure::Budget);
                        audit.fail(StatusCode::FORBIDDEN.as_u16(), GatewayFailure::Budget);
                        return api_error(
                            StatusCode::FORBIDDEN,
                            "permission_error",
                            format!(
                                "colonizer gateway: colony {colony} passed its budget while its request waited; raise the budget and resume it"
                            ),
                            None,
                        );
                    }
                    Some(permit)
                }
                _ => {
                    usage.add_failure_with_fallback(&provider, GatewayFailure::QueueFull);
                    audit.fail(StatusCode::SERVICE_UNAVAILABLE.as_u16(), GatewayFailure::QueueFull);
                    return api_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "overloaded_error",
                        format!("provider \"{id}\" is busy: no free request slot within {queue_timeout} s"),
                        Some("queue_timeout"),
                    );
                }
            }
        }
    };
    let in_flight = Counted::new(&stats.in_flight);
    // The timer starts with the slot in hand, not before the wait for it: a request that queues out must
    // not add its queue time to `duration_ms`, which measures dispatched time only.
    let timed = Timed::new(usage.clone());

    let request = app.gateway.client.request(method, &url).headers(upstream_headers).body(body);
    let upstream = match tokio::time::timeout(timeout, request.send()).await {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => {
            let reason = if e.is_connect() {
                "connection failed"
            } else {
                "request failed"
            };
            usage.add_failure_with_fallback(&provider, GatewayFailure::Unreachable);
            audit.fail(StatusCode::BAD_GATEWAY.as_u16(), GatewayFailure::Unreachable);
            return api_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("provider \"{id}\" is unreachable ({reason}: {})", e.without_url()),
                Some("unreachable"),
            );
        }
        Err(_) => {
            usage.add_failure_with_fallback(&provider, GatewayFailure::Timeout);
            audit.fail(StatusCode::GATEWAY_TIMEOUT.as_u16(), GatewayFailure::Timeout);
            return api_error(
                StatusCode::GATEWAY_TIMEOUT,
                "api_error",
                format!("provider \"{id}\" did not respond within {} s", timeout.as_secs()),
                Some("timeout"),
            );
        }
    };

    // The in-flight guards live as long as the body, covering slots and activity over the whole
    // streamed response; the spend reservation deliberately lives longer, inside the recorder,
    // until the real cost has replaced the estimate.
    let guards = (busy, in_flight, permit, timed);
    let shape = match handling {
        Routed::Translated(info) => {
            let record = usage_recorder(&app, &colony, &provider, reservation);
            // The fallback is decided here, where the provider's `fallback_model` is in reach: set means
            // quota failover is on for this role, unset opts it out, and the env opts out globally.
            let quota_fallback =
                provider.fallback_model.as_deref().is_some_and(|m| !m.is_empty()) && Gateway::quota_fallback_enabled();
            return openai_response(
                upstream,
                guards,
                usage,
                record,
                timeout,
                &info,
                &app.gateway,
                &provider,
                quota_fallback,
                &id,
                Some((&app, &colony)),
                Some(audit),
            )
            .await;
        }
        Routed::Anthropic => Shape::Anthropic,
        Routed::Openai => Shape::Openai,
    };

    let status = upstream.status();
    if status.as_u16() >= 400 {
        // Buffered, not streamed: the body still forwards verbatim, but only a buffered error can
        // be classified for quota exhaustion before answering.
        return anthropic_error(&app, &colony, upstream, guards, reservation, usage, &provider, timeout, audit).await;
    }
    // A 2xx from upstream proves the plan is back: a quota record from an earlier error lapses now,
    // so the queue unpauses and parked colonies resume on the next tick.
    app.gateway.clear_quota_on_success(&id);
    clear_model_error(&app, &colony).await;
    let mut response_headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if !DROP_RESPONSE_HEADERS.contains(&name.as_str()) {
            response_headers.append(name.clone(), value.clone());
        }
    }
    // A GGUF server can sit silent for minutes during prefill before its first SSE event; stream_body
    // keeps the connection alive with comment lines in that case, which only makes sense for SSE:
    // injecting bytes into a non-streaming JSON body would corrupt it.
    let is_sse = response_headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.contains("text/event-stream"));
    // The body forwards exactly as upstream sent it; the tap only watches a private copy for usage,
    // reading the wire the request rode in on. The audit rides in the body: its line lands when the
    // body ends (or is dropped), so the record counts the whole streamed response.
    audit.set_status(status.as_u16());
    let body = counted_body(
        stream_body(upstream.bytes_stream(), guards, timeout, is_sse),
        UsageTap::new(shape, is_sse),
        Some(usage_recorder(&app, &colony, &provider, reservation)),
        Some(audit),
    );
    let mut response = Response::new(Body::from_stream(body));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}

/// A passthrough response's error, buffered whole: error bodies are small and terminal, and
/// only a buffered error can be classified before answering. The body forwards verbatim — an
/// OpenAI-wire route's error stays OpenAI-shaped, no Anthropic wrapping; a quota
/// hit additionally records the provider as exhausted and names it in headers, offering the Claude
/// fallback exactly when the provider has a `fallback_model` (unset is the per-role opt-out) and
/// `COLONIZER_QUOTA_FALLBACK` keeps failover on.
#[allow(clippy::too_many_arguments)]
async fn anthropic_error(
    app: &Shared,
    colony: &str,
    upstream: reqwest::Response,
    guards: Guards,
    reservation: Reserved,
    usage: Arc<UsageCounters>,
    provider: &Provider,
    timeout: Duration,
    audit: GatewayAudit,
) -> Response {
    let status = upstream.status();
    audit.set_status(status.as_u16());
    let mut response_headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if !DROP_RESPONSE_HEADERS.contains(&name.as_str()) {
            response_headers.append(name.clone(), value.clone());
        }
    }
    let bytes = match tokio::time::timeout(timeout, upstream.bytes()).await {
        Ok(Ok(bytes)) => bytes,
        // Past the headers the failure is one unpriced error either way; the distinction the
        // request path draws (unreachable vs timeout) no longer applies.
        _ => {
            usage.add_failure(GatewayFailure::BodyReadFailed);
            audit.fail(StatusCode::BAD_GATEWAY.as_u16(), GatewayFailure::BodyReadFailed);
            return api_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("provider \"{}\" response failed", provider.id),
                None,
            );
        }
    };
    drop(guards);
    // Errors carry no usage, but the recorder ran on them when they streamed past — keep it fed.
    // A record with neither cost nor tokens never lands: `record_routed_usage` early-returns and
    // the unrun closure drops the reservation, releasing it via `Reserved::drop`.
    usage_recorder(app, colony, provider, reservation)(Usage::default());
    let body: Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let kind = body["error"]["type"].as_str().unwrap_or("api_error");
    let message = body["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned());
    let quota = provider_quota::classify_quota_exhaustion(status.as_u16(), kind, &message);
    if let Some(hit) = &quota {
        app.gateway
            .mark_quota_exhausted(&provider.id, hit.reset_at.clone(), hit.reset_unix);
    } else {
        // Named, not invisible: a non-quota 4xx/5xx flags the colony for attention instead of
        // leaving it idle with a failed turn. Quota hits skip this: parking the colony and
        // pausing the queue already say what is wrong.
        eprintln!("gateway: provider \"{}\" answered {status} for colony {colony}", provider.id);
        flag_model_error(app, colony).await;
    }
    // Any fallback: a Claude one the colony's router retries, or a provider-prefixed one the
    // gateway retries itself in [`proxy`] (issue #767).
    let fallback =
        quota.is_some() && provider.fallback_model.as_deref().is_some_and(|m| !m.is_empty()) && Gateway::quota_fallback_enabled();
    if quota.is_some() && !fallback {
        // No retry is coming, so the colony is blocked on this provider (#760, #767).
        app.gateway.note_colony_quota(colony, &provider.id);
    }
    if quota.is_some() {
        audit.fail_with(status.as_u16(), GatewayFailure::QuotaExhausted, fallback);
        if fallback {
            usage.add_failure_with_fallback(provider, GatewayFailure::QuotaExhausted);
        } else {
            usage.add_failure(GatewayFailure::QuotaExhausted);
        }
    } else {
        audit.fail(status.as_u16(), GatewayFailure::UpstreamError);
        usage.add_failure(GatewayFailure::UpstreamError);
    }
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    if let Some(hit) = quota {
        response
            .headers_mut()
            .insert(HeaderName::from_static(QUOTA_HEADER), quota_header_value(&hit));
        if fallback && provider.claude_fallback().is_some() {
            response.headers_mut().insert(
                HeaderName::from_static(FALLBACK_HEADER),
                HeaderValue::from_static(provider_quota::QUOTA_FALLBACK),
            );
        }
    }
    response
}

/// The quota header's value: the reset words when the error named one, plain exhaustion otherwise.
fn quota_header_value(hit: &provider_quota::QuotaExhaustion) -> HeaderValue {
    hit.reset_at
        .as_deref()
        .and_then(|reset| HeaderValue::from_str(reset).ok())
        .unwrap_or_else(|| HeaderValue::from_static("exhausted"))
}

/// An `openai`-wire provider's response in Anthropic's shape. Only `retry-after` is copied from upstream:
/// OpenAI's other headers (`openai-*`, `x-ratelimit-*`) describe a different API. Once response headers
/// have arrived there is no transport fallback — except quota exhaustion, which names
/// `x-colonizer-quota-exhausted` (and the Claude fallback when one is configured). The usage
/// the translation already extracted is teed out to `record_routed_usage` on both paths.
#[allow(clippy::too_many_arguments)]
pub(super) async fn openai_response(
    upstream: reqwest::Response,
    guards: Guards,
    usage: Arc<UsageCounters>,
    record: Recorder,
    timeout: Duration,
    info: &openai::RequestInfo,
    gateway: &Gateway,
    provider: &Provider,
    quota_fallback: bool,
    id: &str,
    // Who to flag for attention on an upstream 4xx/5xx; `None` in tests, which have no session store.
    attention: Option<(&Shared, &str)>,
    // The request's audit record; `None` in tests that call this handler directly.
    audit: Option<GatewayAudit>,
) -> Response {
    let status = upstream.status();
    let retry_after = upstream.headers().get("retry-after").cloned();
    if status.is_success() && info.stream {
        // Streaming 2xx headers prove the plan is back, as below.
        gateway.clear_quota_on_success(id);
        if let Some((app, colony)) = attention {
            clear_model_error(app, colony).await;
        }
        if let Some(audit) = &audit {
            audit.set_status(status.as_u16());
        }
        // The translated SSE stream forwards through the same byte counter and audit finish as the
        // anthropic wire; its tokens are already priced inside the translation.
        let body = counted_body(
            stream_body(
                openai::translate_stream(upstream.bytes_stream(), info.model.clone(), record),
                guards,
                timeout,
                true,
            ),
            UsageTap::Skip,
            None,
            audit,
        );
        let mut response = Response::new(Body::from_stream(body));
        response
            .headers_mut()
            .insert("content-type", HeaderValue::from_static("text/event-stream"));
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-cache"));
        return response;
    }
    let bytes = match tokio::time::timeout(timeout, upstream.bytes()).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            usage.add_failure(GatewayFailure::BodyReadFailed);
            if let Some(audit) = &audit {
                audit.fail(StatusCode::BAD_GATEWAY.as_u16(), GatewayFailure::BodyReadFailed);
            }
            return api_error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("provider \"{id}\" response failed: {}", e.without_url()),
                None,
            );
        }
        Err(_) => {
            usage.add_failure(GatewayFailure::BodyReadFailed);
            if let Some(audit) = &audit {
                audit.fail(StatusCode::GATEWAY_TIMEOUT.as_u16(), GatewayFailure::BodyReadFailed);
            }
            return api_error(
                StatusCode::GATEWAY_TIMEOUT,
                "api_error",
                format!("provider \"{id}\" did not finish its response within {} s", timeout.as_secs()),
                None,
            );
        }
    };
    drop(guards);
    let mut response = if status.is_success() {
        match openai::translate_response(&bytes, info) {
            Ok((message, priced)) => {
                // A translated 2xx proves the plan is back: the quota record lapses now.
                gateway.clear_quota_on_success(id);
                if let Some((app, colony)) = attention {
                    clear_model_error(app, colony).await;
                }
                if let Some(audit) = &audit {
                    audit.set_status(StatusCode::OK.as_u16());
                    audit.add_bytes(bytes.len());
                    audit.finish(priced);
                }
                record(priced);
                (StatusCode::OK, Json(message)).into_response()
            }
            Err(message) => {
                if let Some(audit) = &audit {
                    audit.fail(StatusCode::BAD_GATEWAY.as_u16(), GatewayFailure::BodyReadFailed);
                }
                api_error(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    format!("provider \"{id}\": {message}"),
                    None,
                )
            }
        }
    } else {
        let upstream_status = status.as_u16();
        let (status, kind, message) = openai::translate_error(status, &bytes, id);
        // The translation drops the provider's error code (`insufficient_quota` becomes 403
        // `permission_error`), so the classifier reads the raw code, not the translated kind.
        let parsed: Value = serde_json::from_slice(&bytes).unwrap_or_default();
        let code = parsed["error"]["code"].as_str().unwrap_or(kind);
        // The translated body keeps the provider's detail, so the classifier reads the translation.
        match provider_quota::classify_quota_exhaustion(upstream_status, code, &message) {
            Some(hit) => {
                gateway.mark_quota_exhausted(id, hit.reset_at.clone(), hit.reset_unix);
                if !quota_fallback && let Some((_, colony)) = attention {
                    gateway.note_colony_quota(colony, id);
                }
                if let Some(audit) = &audit {
                    audit.fail_with(status.as_u16(), GatewayFailure::QuotaExhausted, quota_fallback);
                }
                if quota_fallback {
                    usage.add_failure_with_fallback(provider, GatewayFailure::QuotaExhausted);
                } else {
                    usage.add_failure(GatewayFailure::QuotaExhausted);
                }
                let mut response = api_error(
                    status,
                    kind,
                    &message,
                    // The router's licence is for a Claude fallback only; a provider-prefixed one
                    // is the gateway's own retry (issue #767).
                    (quota_fallback && provider.claude_fallback().is_some()).then_some(provider_quota::QUOTA_FALLBACK),
                );
                response
                    .headers_mut()
                    .insert(HeaderName::from_static(QUOTA_HEADER), quota_header_value(&hit));
                response
            }
            None => {
                if upstream_status >= 400 {
                    usage.add_failure(GatewayFailure::UpstreamError);
                    if let Some(audit) = &audit {
                        audit.fail(status.as_u16(), GatewayFailure::UpstreamError);
                    }
                    if let Some((app, colony)) = attention {
                        // Named, not invisible: a 400 here is usually a field the provider's dialect
                        // rejects, and any 4xx/5xx flags the colony for attention instead of leaving
                        // it idle with a failed turn. Quota hits skip this: parking the colony and
                        // pausing the queue already say what is wrong.
                        eprintln!("gateway: provider \"{id}\" answered {upstream_status} for colony {colony}");
                        flag_model_error(app, colony).await;
                    }
                } else if let Some(audit) = &audit {
                    audit.set_status(upstream_status);
                }
                api_error(status, kind, message, None)
            }
        }
    };
    if let Some(value) = retry_after {
        response.headers_mut().insert("retry-after", value);
    }
    response
}
