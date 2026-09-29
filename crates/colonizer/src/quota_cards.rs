//! "Provider out of quota" cards (issue #767, and the core of #760): when a provider's plan runs
//! out, the maintainer gets one dedicated card per provider — not a free-form agent question —
//! naming the provider and model, when the plan resets, and every colony blocked on it, with three
//! answers: switch those colonies to a healthy model, wait (park them and resume them at the reset),
//! or stop them.
//!
//! What a card is built from: the gateway's quota record for the provider (`provider-quota.json`,
//! issue #225), the colonies the gateway saw answered quota-exhausted with no Claude fallback
//! ([`crate::gateway::ColonyQuotaHit`]), and the colonies already parked on that provider. The
//! derivation and the resume schedule are pure, so they are tested with a fixed clock; the handlers
//! only wire them to the session store, the lifecycle handlers and the settings files.

use crate::{
    ApiResult, Shared, client_error,
    config::setting_str,
    config_unreadable,
    gateway::{ColonyQuotaHit, health},
    lifecycle, orgs, provider_quota,
    providers::{self, Provider},
    sessions::{Session, SessionStatus},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The `action` a quota wait stamps on the parked colony's attention flag, beside `resume_unix`.
pub(crate) const WAIT_ACTION: &str = "wait";

/// Whether the colony's attention flag is the quota park/block reason.
fn quota_flagged(s: &Session) -> bool {
    s.attention
        .as_ref()
        .is_some_and(|a| a["reason"].as_str() == Some(provider_quota::QUOTA_EXHAUSTED_REASON))
}

/// The provider a quota-flagged colony is parked or blocked on: the flag's own `provider` when it
/// names one (this module's flags do), else the provider its error text names (events.rs parks).
pub(crate) fn flagged_provider(s: &Session, provider_ids: &[String]) -> Option<String> {
    if !quota_flagged(s) {
        return None;
    }
    if let Some(p) = s.attention.as_ref().and_then(|a| a["provider"].as_str()) {
        return Some(p.to_string());
    }
    provider_quota::mentioned_provider(s.error.as_deref().unwrap_or_default(), provider_ids, &[])
}

/// Still in play: not cleaned up, and live, queued or parked — or the pre-#213 `stopped` park
/// shape, which a quota flag keeps resumable.
fn in_play(s: &Session) -> bool {
    !s.cleaned_up
        && (s.status.is_live()
            || matches!(s.status, SessionStatus::Parked | SessionStatus::Queued)
            || (s.status == SessionStatus::Stopped && quota_flagged(s)))
}

/// The colonies a provider's exhaustion affects: every colony in play the gateway saw blocked on
/// it, or already parked (or flagged) on it.
pub(crate) fn affected<'a>(
    provider: &str,
    sessions: &'a [Session],
    hits: &HashMap<String, ColonyQuotaHit>,
    provider_ids: &[String],
) -> Vec<&'a Session> {
    sessions
        .iter()
        .filter(|s| {
            in_play(s)
                && (hits.get(&s.id).is_some_and(|h| h.provider == provider)
                    || flagged_provider(s, provider_ids).as_deref() == Some(provider))
        })
        .collect()
}

/// The models on `provider` the colonies run, most used first, without the `<provider>/` prefix:
/// what the card's header names.
fn provider_models(provider: &str, colonies: &[&Session]) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for s in colonies {
        let named = s
            .allowed_models
            .iter()
            .flatten()
            .chain(s.model_override.iter())
            .chain(s.subagent_model_override.iter());
        let mut seen = HashSet::new();
        for m in named {
            if let Some(rest) = m.strip_prefix(provider).and_then(|r| r.strip_prefix('/'))
                && !rest.is_empty()
                && seen.insert(rest.to_string())
            {
                *counts.entry(rest.to_string()).or_default() += 1;
            }
        }
    }
    let mut models: Vec<(String, usize)> = counts.into_iter().collect();
    models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    models.into_iter().map(|(m, _)| m).collect()
}

/// When a waiting colony is scheduled back, if a quota wait parked it with a reset time.
fn resume_unix(s: &Session) -> Option<i64> {
    let attention = s.attention.as_ref()?;
    (attention["action"].as_str() == Some(WAIT_ACTION)).then(|| attention["resume_unix"].as_i64())?
}

/// One card per exhausted provider with at least one affected colony. `exhausted` is the gateway's
/// active quota records (`(id, reset_at, reset_unix)`); the Claude account record is not a provider
/// and keeps its own banner. `alternatives` is the model picker, already without exhausted
/// providers; each card drops its own provider's models from it as well.
pub(crate) fn build_cards(
    exhausted: &[(String, Option<String>, Option<i64>)],
    providers: &[Provider],
    sessions: &[Session],
    hits: &HashMap<String, ColonyQuotaHit>,
    alternatives: &[Value],
) -> Vec<Value> {
    let provider_ids: Vec<String> = providers.iter().map(|p| p.id.clone()).collect();
    let mut cards = Vec::new();
    for (id, reset_at, reset_unix) in exhausted {
        let Some(provider) = providers.iter().find(|p| &p.id == id) else {
            continue;
        };
        let colonies = affected(id, sessions, hits, &provider_ids);
        if colonies.is_empty() {
            continue;
        }
        let models = provider_models(id, &colonies);
        let title = match models.first() {
            Some(model) => format!("{id} · {model} is out of quota"),
            None => format!("{id} is out of quota"),
        };
        let orgs: BTreeSet<&str> = colonies.iter().map(|s| s.org.as_str()).collect();
        let waiting: Vec<i64> = colonies.iter().filter_map(|s| resume_unix(s)).collect();
        let rows: Vec<Value> = colonies
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "repo": s.repo,
                    "org": s.org,
                    "issue": s.issue,
                    "issue_title": s.issue_title,
                    "status": s.status,
                    "hits": hits.get(&s.id).filter(|h| &h.provider == id).map(|h| h.hits),
                    "waiting": resume_unix(s).is_some() || (s.status == SessionStatus::Parked && quota_flagged(s)),
                    "resume_unix": resume_unix(s),
                })
            })
            .collect();
        let prefix = format!("{id}/");
        let picker: Vec<&Value> = alternatives
            .iter()
            .filter(|a| !a["id"].as_str().is_some_and(|m| m.starts_with(&prefix)))
            .collect();
        cards.push(json!({
            "provider": id,
            "provider_name": provider.name,
            "models": models,
            "title": title,
            "reset_at": reset_at,
            "reset_unix": reset_unix,
            "colonies": rows,
            "orgs": orgs,
            "waiting": waiting.len(),
            "resume_unix": waiting.iter().min(),
            "fallback_model": provider.fallback_model,
            "alternatives": picker,
        }));
    }
    cards
}

/// The switch picker: every model the mothership offers (`/api/models`) that is not on an exhausted
/// provider, with its provider's health — Claude's models read healthy unless the account's own
/// cap holds. Healthy first, in offer order otherwise.
async fn alternatives(app: &Shared, exhausted: &[(String, Option<String>, Option<i64>)]) -> Vec<Value> {
    let out_ids: HashSet<&str> = exhausted.iter().map(|(id, _, _)| id.as_str()).collect();
    let account_out = app.gateway.is_account_quota_exhausted();
    let mut out: Vec<Value> = providers::ANTHROPIC_MODELS
        .iter()
        .map(|(id, label)| {
            json!({
                "id": id, "label": label, "provider": "anthropic",
                "failure_pct": 0.0, "rated": false, "degraded": account_out, "healthy": !account_out,
            })
        })
        .collect();
    for provider in app.providers() {
        if out_ids.contains(provider.id.as_str()) {
            continue;
        }
        let h = health(&app.gateway.usage(&provider.id));
        for model in &provider.models {
            out.push(json!({
                "id": format!("{}/{model}", provider.id),
                "label": format!("{model} · {}", provider.name),
                "provider": provider.id,
                "failure_pct": h.failure_pct,
                "rated": h.rated,
                "degraded": h.degraded,
                "healthy": !h.degraded,
            }));
        }
    }
    // A stable sort: healthy first, the offer order kept within each half.
    out.sort_by_key(|a| a["healthy"] != true);
    out
}

/// Every provider-out-of-quota card, as `GET /api/attention` and the status poll serve them.
pub(crate) async fn cards(app: &Shared) -> Vec<Value> {
    let exhausted: Vec<_> = app
        .gateway
        .quota_exhausted()
        .into_iter()
        .filter(|(id, _, _)| id != crate::gateway::ACCOUNT_QUOTA_ID)
        .collect();
    if exhausted.is_empty() {
        return Vec::new();
    }
    let picker = alternatives(app, &exhausted).await;
    let sessions = app.sessions.read().await.clone();
    build_cards(
        &exhausted,
        &app.providers(),
        &sessions,
        &app.gateway.colony_quota_all(),
        &picker,
    )
}

/// `GET /api/attention`: what needs the maintainer beyond a colony's own question. Today that is
/// the provider-out-of-quota cards (issue #767).
pub async fn list(State(app): State<Shared>) -> Json<Value> {
    Json(json!({ "quota_cards": cards(&app).await }))
}

/// Whether a quota-parked colony is due back at `now_unix` (the resume scheduler, issue #767): the
/// wait's own resume time has come, or its provider is no longer exhausted — recovered early, or a
/// reset-less record lapsed. `provider_exhausted` is the gateway's verdict for the provider the
/// park names (or for any provider, when it names none).
pub(crate) fn park_due(attention: &Value, provider_exhausted: bool, now_unix: i64) -> bool {
    attention["resume_unix"].as_i64().is_some_and(|at| now_unix >= at) || !provider_exhausted
}

/// Whether a live colony is blocked on an exhausted provider: every request it made since its last
/// success came back quota-exhausted with no fallback, and the provider is still out. What the
/// watchdog flags instead of nudging (issue #760).
pub(crate) fn blocked_on(app: &Shared, colony: &str) -> Option<ColonyQuotaHit> {
    app.gateway
        .colony_quota(colony)
        .filter(|hit| app.gateway.is_quota_exhausted(&hit.provider))
}

/// Flags every live colony blocked on an exhausted provider — `starting` ones included, whose agent
/// never got a turn out — with the quota reason and the provider, so it shows on the provider's
/// card and in "needs you" instead of sitting in `starting` or being nudged into a model that
/// cannot answer (issue #760). Returns the blocked colony ids, which the watchdog then leaves alone.
pub(crate) async fn flag_blocked(app: &Shared) -> HashSet<String> {
    let sessions = app.sessions.read().await.clone();
    let mut blocked = HashSet::new();
    for s in sessions.iter().filter(|s| s.status.is_live() && s.suspended.is_none()) {
        let Some(hit) = blocked_on(app, &s.id) else { continue };
        blocked.insert(s.id.clone());
        let already = s
            .attention
            .as_ref()
            .is_some_and(|a| quota_flagged(s) && a["provider"].as_str() == Some(hit.provider.as_str()));
        if already {
            continue;
        }
        let state = app.gateway.quota_state(&hit.provider);
        let reset_at = state.as_ref().and_then(|q| q.reset_at.clone());
        let reset_unix = state.as_ref().and_then(|q| q.reset_unix);
        app.update_session(&s.id, |x| {
            x.attention = Some(json!({
                "reason": provider_quota::QUOTA_EXHAUSTED_REASON,
                "since": hit.since,
                "nudges": 0,
                "provider": hit.provider,
                "reset_at": reset_at,
                "reset_unix": reset_unix,
            }));
        })
        .await;
        let when = reset_at.map(|r| format!(", resets {r}")).unwrap_or_default();
        let what = if s.status == SessionStatus::Starting {
            "the agent has not produced a turn since boot"
        } else {
            "nothing it asked for since its last answer went through"
        };
        app.session_log(
            &s.id,
            "error",
            format!(
                "provider \"{}\" is out of quota{when} and answered all {} of this colony's requests with quota_exhausted — {what}; this colony needs you: switch its model, wait for the reset, or stop it",
                hit.provider, hit.hits
            ),
        )
        .await;
    }
    blocked
}

#[derive(Deserialize)]
pub struct QuotaActionRequest {
    /// `switch`, `wait` or `stop`.
    action: String,
    /// The model to switch to (`switch` only): a Claude alias/id or `<provider>/<model>`.
    #[serde(default)]
    model: Option<String>,
    /// Where a switch applies: `colonies` (the default — the card's colonies) or `org` (their
    /// orgs' model settings as well, so the next colony there starts on the new model).
    #[serde(default)]
    scope: Option<String>,
    /// Limit the action to these colony ids (all of them must be on the card).
    #[serde(default)]
    colonies: Option<Vec<String>>,
    /// With `org` scope, limit the settings change (and the colonies) to this org.
    #[serde(default)]
    org: Option<String>,
    /// Remember the switch model as this provider's `fallback_model` (a Claude model only).
    #[serde(default)]
    remember: bool,
}

fn bad(message: &str) -> crate::AppError {
    client_error(StatusCode::BAD_REQUEST, message)
}

/// `POST /api/providers/{id}/quota-action`: answers the provider's card for its colonies.
pub async fn quota_action(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<QuotaActionRequest>,
) -> ApiResult<Value> {
    let providers = app.providers();
    let Some(provider) = providers.iter().find(|p| p.id == id).cloned() else {
        return Err(client_error(StatusCode::NOT_FOUND, &format!("no provider \"{id}\"")));
    };
    let provider_ids: Vec<String> = providers.iter().map(|p| p.id.clone()).collect();
    let sessions = app.sessions.read().await.clone();
    let hits = app.gateway.colony_quota_all();
    let mut targets: Vec<Session> = affected(&id, &sessions, &hits, &provider_ids).into_iter().cloned().collect();
    if let Some(only) = &req.colonies {
        if let Some(stray) = only.iter().find(|c| !targets.iter().any(|s| &s.id == *c)) {
            return Err(bad(&format!("colony {stray} is not blocked on provider \"{id}\"")));
        }
        targets.retain(|s| only.contains(&s.id));
    }
    if let Some(org) = &req.org {
        targets.retain(|s| &s.org == org);
    }
    let results = match req.action.as_str() {
        "stop" => stop_all(&app, &targets).await,
        "wait" => wait_all(&app, &provider, &targets).await,
        "switch" => {
            let model = req.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
            let Some(model) = model else {
                return Err(bad("a switch names the model to switch to"));
            };
            check_switch_model(&app, &id, model).await?;
            let scope = req.scope.as_deref().unwrap_or("colonies");
            if !matches!(scope, "colonies" | "org") {
                return Err(bad(&format!(
                    "scope {scope:?} is not supported; use \"colonies\" or \"org\" (every-role switching is not built yet)"
                )));
            }
            if req.remember && (model.contains('/') || !providers::valid_model(model)) {
                return Err(bad(
                    "remember sets the provider's fallback_model, which must be a Claude model (no provider prefix)",
                ));
            }
            if req.remember {
                remember_fallback(&app, &id, model).await?;
            }
            if scope == "org" {
                let orgs: BTreeSet<String> = targets.iter().map(|s| s.org.clone()).collect();
                switch_orgs(&app, &id, model, &orgs).await?;
            }
            switch_all(&app, &id, model, &targets).await
        }
        other => return Err(bad(&format!("unknown action {other:?}; use switch, wait or stop"))),
    };
    let failed: Vec<&Value> = results.iter().filter(|r| r["ok"] != true).collect();
    Ok(Json(json!({
        "action": req.action,
        "provider": id,
        "colonies": results.iter().filter(|r| r["ok"] == true).map(|r| r["id"].clone()).collect::<Vec<_>>(),
        "failed": failed,
    })))
}

fn outcome(id: &str, result: Result<(), String>) -> Value {
    match result {
        Ok(()) => json!({"id": id, "ok": true}),
        Err(error) => json!({"id": id, "ok": false, "error": error}),
    }
}

async fn stop_all(app: &Shared, targets: &[Session]) -> Vec<Value> {
    let mut out = Vec::new();
    for s in targets {
        app.gateway.clear_colony_quota(&s.id);
        let result = lifecycle::stop(State(app.clone()), Path(s.id.clone()))
            .await
            .map(|_| ())
            .map_err(|e| e.message());
        if result.is_ok() {
            // The pre-#213 stopped park shape is already stopped: its quota flag is its resume
            // ticket, so the stop takes the ticket away.
            app.update_session(&s.id, |x| {
                if x.status.is_terminal() && quota_flagged(x) {
                    x.attention = None;
                }
            })
            .await;
            app.session_log(&s.id, "info", "stopped from the provider-out-of-quota card".into())
                .await;
        }
        out.push(outcome(&s.id, result));
    }
    out
}

/// Parks each colony on the provider until its reset: live ones through [`lifecycle::park_colony`]
/// (suspended, not failed — the worktree kept), already-parked ones re-stamped. The flag carries the
/// provider and `resume_unix`, which the queue's resume pass ([`park_due`]) reads.
async fn wait_all(app: &Shared, provider: &Provider, targets: &[Session]) -> Vec<Value> {
    let state = app.gateway.quota_state(&provider.id);
    let reset_at = state.as_ref().and_then(|q| q.reset_at.clone());
    let reset_unix = state.as_ref().and_then(|q| q.reset_unix);
    let error = match &reset_at {
        Some(reset) => format!("provider quota exhausted ({}, resets {reset})", provider.id),
        None => format!("provider quota exhausted ({})", provider.id),
    };
    let mut out = Vec::new();
    for s in targets {
        app.gateway.clear_colony_quota(&s.id);
        if s.status == SessionStatus::Queued {
            out.push(outcome(
                &s.id,
                Err("queued, not started: the queue holds it while the provider is out".into()),
            ));
            continue;
        }
        if s.status.is_live() {
            lifecycle::park_colony(
                app,
                s,
                provider_quota::QUOTA_EXHAUSTED_REASON,
                reset_at.clone(),
                error.clone(),
                "provider out of quota: parked from its card until the plan resets; it resumes on its own".into(),
            )
            .await;
        }
        let stamped = app
            .update_session(&s.id, |x| {
                let parked = x.status == SessionStatus::Parked || (x.status == SessionStatus::Stopped && quota_flagged(x));
                if parked {
                    x.error = Some(error.clone());
                    x.attention = Some(json!({
                        "reason": provider_quota::QUOTA_EXHAUSTED_REASON,
                        "since": Utc::now(),
                        "nudges": 0,
                        "provider": provider.id,
                        "action": WAIT_ACTION,
                        "reset_at": reset_at,
                        "resume_unix": reset_unix,
                    }));
                }
                parked
            })
            .await
            .is_some_and(|(_, parked)| parked);
        out.push(outcome(
            &s.id,
            if stamped {
                Ok(())
            } else {
                Err(format!("could not park it (status {})", s.status.as_str()))
            },
        ));
    }
    out
}

/// A switch target must be a model the mothership offers, and not on an exhausted provider.
async fn check_switch_model(app: &Shared, provider: &str, model: &str) -> Result<(), crate::AppError> {
    if providers::names_model_on(model, provider) {
        return Err(bad(&format!(
            "{model} is on \"{provider}\", the provider that is out of quota"
        )));
    }
    let exhausted: Vec<_> = app.gateway.quota_exhausted();
    let offered = alternatives(app, &exhausted).await;
    if !offered.iter().any(|a| a["id"] == model) {
        return Err(bad(&format!(
            "{model} is not a model on offer, or its provider is out of quota too; pick one from the card"
        )));
    }
    Ok(())
}

/// The role models a colony boots with — `(model, subagent_model, background_model)` — resolved the
/// way boot does: its launch overrides, else its org's settings, else the agent module's defaults.
fn role_models(app: &Shared, modules: &crate::config::ModulesConfig, s: &Session) -> [Option<String>; 3] {
    let org = app.org_settings(&s.org);
    let agent_id = if s.agent.is_empty() {
        modules.agent.provider.clone()
    } else {
        s.agent.clone()
    };
    let choice = orgs::effective_agent_for(modules, &org, &agent_id);
    let schema = app
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .map(|a| a.schema.clone())
        .unwrap_or(Value::Null);
    let pick = |key: &str| Some(setting_str(&choice, &schema, key)).filter(|m| !m.is_empty());
    [
        s.model_override.clone().or_else(|| pick("model")),
        s.subagent_model_override.clone().or_else(|| pick("subagent_model")),
        pick("background_model"),
    ]
}

/// Points every colony's role on `provider` at `model` and restarts it on the new model scope: the
/// orchestrator and subagent overrides are the per-colony settings boot reads, and boot re-derives
/// `allowed_models` from them (#727), so the restart is what lets the gateway admit the new model.
/// A colony with no role visibly on the provider (a tier or background model routed there) gets the
/// orchestrator override, the role that hit the quota most often.
async fn switch_all(app: &Shared, provider: &str, model: &str, targets: &[Session]) -> Vec<Value> {
    let modules = app.modules.read().await.clone();
    let mut out = Vec::new();
    for s in targets {
        let [main, sub, _] = role_models(app, &modules, s);
        let on = |m: &Option<String>| m.as_deref().is_some_and(|m| providers::names_model_on(m, provider));
        let (main_on, sub_on) = (on(&main), on(&sub));
        app.update_session(&s.id, |x| {
            if main_on || !sub_on {
                x.model_override = Some(model.to_string());
            }
            if sub_on {
                x.subagent_model_override = Some(model.to_string());
            }
        })
        .await;
        app.gateway.clear_colony_quota(&s.id);
        app.session_log(
            &s.id,
            "info",
            format!("provider \"{provider}\" is out of quota: switched to {model} from its card; restarting on the new model"),
        )
        .await;
        out.push(outcome(&s.id, restart(app, &s.id).await));
    }
    out
}

/// Restarts a colony so its next boot reads its new model settings: a live or parked one is stopped
/// (its microVM taken down, the worktree kept) and resumed cold; a stopped one is resumed; a queued
/// one boots with them anyway.
async fn restart(app: &Shared, id: &str) -> Result<(), String> {
    let Some(s) = app.session(id).await else {
        return Err("no such session".into());
    };
    if s.status == SessionStatus::Queued {
        return Ok(());
    }
    if s.status.is_live() || s.status == SessionStatus::Parked {
        lifecycle::stop(State(app.clone()), Path(id.to_string()))
            .await
            .map(|_| ())
            .map_err(|e| e.message())?;
    }
    lifecycle::resume(State(app.clone()), Path(id.to_string()), None)
        .await
        .map(|_| ())
        .map_err(|e| e.message())
}

/// `org` scope: in each org, every model setting whose value routes to `provider` now names `model`,
/// so the org's next colonies start on it too.
async fn switch_orgs(
    app: &Shared,
    provider: &str,
    model: &str,
    orgs_to_change: &BTreeSet<String>,
) -> Result<(), crate::AppError> {
    let modules = app.modules.read().await.clone();
    let _config = app.config_write.lock().await;
    let mut all: BTreeMap<String, orgs::OrgSettings> =
        crate::util::read_json_or_default(&app.orgs_file()).map_err(|e| config_unreadable(&app.orgs_file(), &e))?;
    for org in orgs_to_change {
        let mut settings = all.get(org).cloned().unwrap_or_default();
        let agent_id = orgs::effective_agent_module(&settings, &modules);
        let choice = orgs::effective_agent_for(&modules, &settings, &agent_id);
        let schema = app
            .agents
            .iter()
            .find(|a| a.id == agent_id)
            .map(|a| a.schema.clone())
            .unwrap_or(Value::Null);
        let on = |key: &str| providers::names_model_on(&setting_str(&choice, &schema, key), provider);
        let (main, sub, background) = (on("model"), on("subagent_model"), on("background_model"));
        if !(main || sub || background) {
            continue;
        }
        let agent = settings.agent.get_or_insert_with(Default::default);
        if main {
            agent.model = Some(model.to_string());
        }
        if sub {
            agent.subagent_model = Some(model.to_string());
        }
        if background {
            agent.background_model = Some(model.to_string());
        }
        all.insert(org.clone(), settings);
    }
    app.save_org_settings(&all).await?;
    Ok(())
}

/// `remember`: the provider's `fallback_model`, so the next exhaustion retries on Claude by itself.
async fn remember_fallback(app: &Shared, provider: &str, model: &str) -> Result<(), crate::AppError> {
    let _config = app.config_write.lock().await;
    let mut list: Vec<Provider> =
        crate::util::read_json_or_default(&app.providers_file()).map_err(|e| config_unreadable(&app.providers_file(), &e))?;
    let Some(entry) = list.iter_mut().find(|p| p.id == provider) else {
        return Err(client_error(StatusCode::NOT_FOUND, &format!("no provider \"{provider}\"")));
    };
    entry.fallback_model = Some(model.to_string());
    app.save_providers(&list).await?;
    Ok(())
}

/// The API routes this module serves; `server::api_routes` merges them.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/attention", routing::get(list))
        .route("/api/providers/{id}/quota-action", routing::post(quota_action))
}

#[cfg(test)]
mod tests;
