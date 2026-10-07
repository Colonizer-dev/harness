//! The Claude account fallback (issue #1130): while the Claude subscription is out, a colony's
//! requests that resolve to a Claude model go to the install's `account_fallback_model` instead,
//! and go back to Claude by themselves when the account's record lapses.
//!
//! Claude's own traffic does not pass through the gateway, so the colony's model router
//! (`router.mjs`) asks this route — `GET /account-route`, authenticated like every gateway route —
//! before it forwards an unrouted request to Anthropic. The decision is made here, at request time,
//! from the account's quota record and the setting as they stand: no saved setting changes, so there
//! is nothing to switch back at the reset. [`decide`] is the pure rule; the handler reads the state
//! and tells the colony's log once per change.
//!
//! Restricted work is held to the same bar as any request the gateway carries: the fallback is used
//! only when its provider meets the task's sensitivity class. Otherwise the answer is
//! [`AccountRoute::Parked`], the router leaves the request on Claude, the turn fails on the quota as
//! it does today, and the colony parks with the reason this decision names.

use super::*;
use crate::{
    sensitivity::{self, Sensitivity, SensitivityOverrides},
    sessions::Session,
};

/// What a colony's Claude request does right now.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AccountRoute {
    /// Claude answers: the account is fine, no fallback is set, or the fallback cannot take it.
    Claude,
    /// The account is out: route to `model` (`<provider>/<model>`) until the account resets.
    Fallback {
        model: String,
        provider_name: String,
        reset_at: Option<String>,
        reset_unix: Option<i64>,
    },
    /// The account is out and the fallback may not carry this colony's task.
    Parked { reason: String },
}

impl AccountRoute {
    fn kind(&self) -> &'static str {
        match self {
            AccountRoute::Claude => "claude",
            AccountRoute::Fallback { .. } => "fallback",
            AccountRoute::Parked { .. } => "parked",
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        match self {
            AccountRoute::Claude => json!({"action": self.kind()}),
            AccountRoute::Fallback {
                model,
                provider_name,
                reset_at,
                reset_unix,
            } => json!({
                "action": self.kind(), "model": model, "provider_name": provider_name,
                "reset_at": reset_at, "reset_unix": reset_unix,
            }),
            AccountRoute::Parked { reason } => json!({"action": self.kind(), "reason": reason}),
        }
    }
}

/// "until 19:51" for a reset the account's error named, or from its unix time; empty when neither.
pub(crate) fn until_words(reset_at: Option<&str>, reset_unix: Option<i64>) -> String {
    match (reset_at, reset_unix.and_then(|t| DateTime::from_timestamp(t, 0))) {
        (Some(reset), _) => format!(" until {reset}"),
        (None, Some(at)) => format!(" until {} UTC", at.format("%H:%M")),
        _ => String::new(),
    }
}

/// The pure rule. `account` is the account's quota record while it holds (`None` when the account
/// works), `fallback_model` the setting, `sensitivity` the colony's task class (`None` is an
/// unclassified task, which any provider may carry).
pub(crate) fn decide(
    account: Option<&QuotaState>,
    fallback_model: &str,
    providers: &[crate::providers::Provider],
    provider_out: &dyn Fn(&str) -> bool,
    sensitivity: Option<Sensitivity>,
    overrides: Option<&SensitivityOverrides>,
) -> AccountRoute {
    let Some(account) = account else { return AccountRoute::Claude };
    let Some(provider) = usable_provider(fallback_model, providers, provider_out) else {
        return AccountRoute::Claude;
    };
    if let Some(class) = sensitivity
        && !crate::providers::model_eligible(class, overrides, providers, fallback_model)
    {
        let required = sensitivity::required_mark(class, overrides);
        let until = until_words(account.reset_at.as_deref(), account.reset_unix);
        let name = display_name(provider);
        let why = if sensitivity::ProviderMark::of(provider.trusted, provider.vetted) < required {
            format!("{name} is not marked {}", required.as_str())
        } else {
            format!("{name} is not on this org's restricted-vendor list")
        };
        return AccountRoute::Parked {
            reason: format!("needs a {} provider: Claude is out{until}; {why}", required.as_str()),
        };
    }
    AccountRoute::Fallback {
        model: fallback_model.to_string(),
        provider_name: display_name(provider),
        reset_at: account.reset_at.clone(),
        reset_unix: account.reset_unix,
    }
}

fn display_name(provider: &crate::providers::Provider) -> String {
    let name = provider.name.trim();
    if name.is_empty() {
        provider.id.clone()
    } else {
        name.to_string()
    }
}

/// The provider a fallback setting names, when it is one the gateway can route to right now: a
/// `<provider>/<model>` on a configured provider whose own plan is not out. A bare Claude model, an
/// unconfigured prefix or a provider that is out leaves the fallback unusable, which reads as no
/// fallback: today's behaviour.
pub(crate) fn usable_provider<'a>(
    fallback_model: &str,
    providers: &'a [crate::providers::Provider],
    provider_out: &dyn Fn(&str) -> bool,
) -> Option<&'a crate::providers::Provider> {
    providers
        .iter()
        .find(|p| crate::providers::names_model_on(fallback_model, &p.id))
        .filter(|p| !provider_out(&p.id))
}

/// The install's `account_fallback_model`, trimmed; empty when none is set.
pub(crate) async fn configured_model(app: &App) -> String {
    let modules = app.modules.read().await.clone();
    let schema = crate::modules::schema_for("agent", &modules.agent.provider, &app.agents);
    crate::config::setting_str(&modules.agent, &schema, "account_fallback_model")
        .trim()
        .to_string()
}

/// The route for one colony right now, read off the live state.
pub(crate) async fn route_for(app: &Shared, session: &Session) -> AccountRoute {
    let account = app
        .gateway
        .account_quota_state()
        .filter(|_| app.gateway.is_account_quota_exhausted());
    if account.is_none() {
        return AccountRoute::Claude;
    }
    let model = configured_model(app).await;
    if model.is_empty() {
        return AccountRoute::Claude;
    }
    let overrides = app.org_settings(&session.org).sensitivity;
    decide(
        account.as_ref(),
        &model,
        &app.providers(),
        &|id| app.gateway.is_quota_exhausted(id),
        session.sensitivity.as_deref().and_then(Sensitivity::parse),
        overrides.as_ref(),
    )
}

/// Whether the fallback would carry colonies at all right now (any sensitivity): what the queue and
/// the status poll read to stop treating the account's cap as a pause.
pub(crate) async fn fallback_usable(app: &Shared) -> Option<(String, String)> {
    if !app.gateway.is_account_quota_exhausted() {
        return None;
    }
    let model = configured_model(app).await;
    let providers = app.providers();
    let provider = usable_provider(&model, &providers, &|id| app.gateway.is_quota_exhausted(id))?;
    Some((model.clone(), display_name(provider)))
}

impl Gateway {
    /// Remembers what a colony's Claude requests were last told and says whether that changed: the
    /// handler logs on a change only, so a colony's log gets one line per switch, not one per request.
    fn account_note(&self, colony: &str, route: &AccountRoute) -> bool {
        let mut notes = self.account_notes.lock().unwrap();
        let before = notes.get(colony).cloned();
        match route {
            AccountRoute::Claude => {
                notes.remove(colony);
            }
            other => {
                notes.insert(colony.to_string(), other.clone());
            }
        }
        before.as_ref().map_or("claude", AccountRoute::kind) != route.kind()
    }

    /// Why the fallback could not carry this colony, if its last Claude request was told so: the
    /// park's reason, so the card names it.
    pub(crate) fn account_park_reason(&self, colony: &str) -> Option<String> {
        match self.account_notes.lock().unwrap().get(colony) {
            Some(AccountRoute::Parked { reason }) => Some(reason.clone()),
            _ => None,
        }
    }
}

/// `GET /account-route`: what this colony's Claude requests do right now. Authenticated with the
/// colony token like every gateway route; the answer is the [`AccountRoute`] as JSON.
pub(super) async fn account_route(State(app): State<Shared>, headers: HeaderMap) -> Response {
    let token = headers
        .get(COLONY_HEADER)
        .and_then(|v| v.to_str().ok())
        .or_else(|| bearer_token(&headers))
        .unwrap_or_default();
    let Some(session) = app.colony_for_token(token).await else {
        return api_error(StatusCode::UNAUTHORIZED, "authentication_error", "unknown colony token", None);
    };
    let route = route_for(&app, &session).await;
    if app.gateway.account_note(&session.id, &route) {
        let (level, line) = match &route {
            AccountRoute::Fallback {
                model,
                provider_name,
                reset_at,
                reset_unix,
            } => (
                "info",
                format!(
                    "Claude account is out; this colony's Claude requests run on {model} ({provider_name}){}",
                    until_words(reset_at.as_deref(), *reset_unix)
                ),
            ),
            AccountRoute::Parked { reason } => (
                "warn",
                format!("Claude account is out and the fallback cannot take this task: {reason}"),
            ),
            AccountRoute::Claude => (
                "info",
                "Claude account is back; this colony's requests are on Claude again".to_string(),
            ),
        };
        app.session_log(&session.id, level, line).await;
    }
    Json(route.to_json()).into_response()
}

#[cfg(test)]
mod tests;
