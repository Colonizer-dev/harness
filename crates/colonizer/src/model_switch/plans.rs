//! Plan usage for the header's model switcher: one row per plan the install's model roles route to —
//! the Claude account and each provider in use — with what the mothership actually knows about it.
//!
//! Nothing here is estimated. The rows read the same records the quota pause and the providers
//! screen read: the gateway's quota record (exhausted, its reset, and when the limit was hit), its
//! cumulative request counts, and the plan balance a provider's quota probe (`quota.url`/`pointer`,
//! issue #199) answered — through the probe cache, so opening the popover probes at most once a
//! minute per provider. A plan that reports no balance says so: the cockpit labels what it shows.

use crate::{
    Shared,
    gateway::{ProviderUsage, QuotaState, probe_cached},
    providers::{self, Provider},
};
use axum::{Json, extract::State};
use chrono::Utc;
use serde_json::{Value, json};

/// The id the Claude account's row goes by, as in the status poll's `provider_details`.
pub(crate) const CLAUDE_PLAN_ID: &str = "anthropic";

/// Everything one plan row is built from.
pub(crate) struct PlanInput {
    pub id: String,
    pub name: String,
    /// `claude` for the account's own caps, `provider` for a routed provider.
    pub kind: &'static str,
    /// Plain-word roles routed here (`orchestrator`, `subagents`, …).
    pub used_by: Vec<&'static str>,
    /// The plan is out right now: a recorded limit whose reset is ahead (or whose TTL holds).
    pub exhausted: bool,
    /// The gateway's last quota record, current or lapsed: when the limit was hit and its reset.
    pub record: Option<QuotaState>,
    /// The gateway's request counts; `None` for the Claude account, which the gateway does not proxy.
    pub usage: Option<ProviderUsage>,
    /// The cached quota probe's answer (`{remaining, limit?, error}`) with its `checked_at`; `None`
    /// when the provider has no probe configured.
    pub balance: Option<Value>,
}

/// One plan as `GET /api/models/plans` lists it. `balance.remaining`/`balance.limit` are the
/// probe's own numbers; `pct_left` is derived only when both are known.
pub(crate) fn plan_json(input: &PlanInput) -> Value {
    let record = input.record.as_ref();
    let balance = input.balance.as_ref().map(|b| {
        let remaining = b["remaining"].as_f64();
        let limit = b["limit"].as_f64().filter(|l| *l > 0.0);
        let pct_left = remaining
            .zip(limit)
            .map(|(r, l)| ((r / l * 100.0).clamp(0.0, 100.0) * 10.0).round() / 10.0);
        json!({
            "remaining": b["remaining"],
            "limit": b.get("limit").cloned().unwrap_or(Value::Null),
            "pct_left": pct_left,
            "error": b["error"],
            "checked_at": b.get("checked_at").cloned().unwrap_or(Value::Null),
        })
    });
    json!({
        "id": input.id,
        "name": input.name,
        "kind": input.kind,
        "used_by": input.used_by,
        "exhausted": input.exhausted,
        "reset_at": if input.exhausted { record.and_then(|r| r.reset_at.clone()) } else { None },
        "reset_unix": if input.exhausted { record.and_then(|r| r.reset_unix) } else { None },
        // The last limit the gateway saw, even once lapsed: when it hit and when it said it resets.
        "last_limit": record.map(|r| json!({"at": r.since, "reset_at": r.reset_at, "reset_unix": r.reset_unix})),
        "requests": input.usage.as_ref().map(|u| u.requests),
        "failures": input.usage.as_ref().map(|u| u.failures),
        "last_request_at": input.usage.as_ref().and_then(|u| u.last_request_at),
        "since": input.usage.as_ref().and_then(|u| u.since),
        "balance": balance,
    })
}

/// The plan balance from the probe cache (refreshed when older than the probe TTL), with the
/// time the answer was read. `None` without a probe configured.
async fn balance(app: &Shared, provider: &Provider) -> Option<Value> {
    provider.quota.as_ref()?;
    let health = probe_cached(app, provider).await;
    let mut quota = health
        .get("quota")
        .cloned()
        .unwrap_or_else(|| json!({"remaining": null, "error": "no answer"}));
    quota["checked_at"] = health.get("checked_at").cloned().unwrap_or(Value::Null);
    Some(quota)
}

/// `GET /api/models/plans`: every plan in use — the Claude account when a role runs on Claude (or
/// its cap is hit), then each provider a role routes to (or whose plan is out) — with its limit
/// state, request counts and plan balance.
pub async fn plans(State(app): State<Shared>) -> Json<Value> {
    let envs = providers::runner_envs(&app).await;
    let all = app.providers();
    let mut rows = Vec::new();
    let claude_roles = providers::claude_used_by(&all, &envs);
    let account_out = app.gateway.is_account_quota_exhausted();
    if !claude_roles.is_empty() || account_out {
        rows.push(plan_json(&PlanInput {
            id: CLAUDE_PLAN_ID.into(),
            name: "Claude".into(),
            kind: "claude",
            used_by: claude_roles,
            exhausted: account_out,
            record: app.gateway.account_quota_state(),
            usage: None,
            balance: None,
        }));
        // While the account is out and its fallback carries the work, the row says where it runs
        // (#1130) — the plan bars read "Claude out, running on MiniMax until 19:51".
        if account_out
            && let Some((model, provider_name)) = crate::gateway::fallback_usable(&app).await
            && let Some(row) = rows.last_mut()
        {
            row["fallback"] = json!({"model": model, "provider_name": provider_name});
        }
    }
    let used: Vec<(&Provider, Vec<&'static str>, bool)> = all
        .iter()
        .map(|p| {
            let roles: Vec<&'static str> = providers::used_by(&p.id, &envs)
                .into_iter()
                .map(providers::role_label)
                .collect();
            (p, roles, app.gateway.is_quota_exhausted(&p.id))
        })
        .filter(|(_, roles, out)| !roles.is_empty() || *out)
        .collect();
    let balances = futures_util::future::join_all(used.iter().map(|(p, _, _)| balance(&app, p))).await;
    for ((provider, roles, exhausted), balance) in used.into_iter().zip(balances) {
        let name = provider.name.trim();
        rows.push(plan_json(&PlanInput {
            id: provider.id.clone(),
            name: if name.is_empty() {
                provider.id.clone()
            } else {
                name.to_string()
            },
            kind: "provider",
            used_by: roles,
            exhausted,
            record: app.gateway.quota_state(&provider.id),
            usage: Some(app.gateway.usage(&provider.id)),
            balance,
        }));
    }
    Json(json!({"plans": rows, "checked_at": Utc::now()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> PlanInput {
        PlanInput {
            id: "byteplus".into(),
            name: "BytePlus".into(),
            kind: "provider",
            used_by: vec!["subagents"],
            exhausted: false,
            record: None,
            usage: None,
            balance: None,
        }
    }

    #[test]
    fn an_exhausted_plan_carries_its_reset_and_when_it_hit() {
        let since = Utc::now();
        let row = plan_json(&PlanInput {
            exhausted: true,
            record: Some(QuotaState {
                reset_at: Some("10-05 19:51:58".into()),
                reset_unix: Some(1_791_229_918),
                since,
            }),
            ..input()
        });
        assert_eq!(row["exhausted"], true);
        assert_eq!(row["reset_unix"], 1_791_229_918);
        assert_eq!(row["last_limit"]["reset_at"], "10-05 19:51:58");
        assert_eq!(row["used_by"], json!(["subagents"]));
        assert_eq!(row["balance"], Value::Null, "no probe, no balance — never a guess");
    }

    #[test]
    fn a_lapsed_limit_is_history_not_a_current_reset() {
        let row = plan_json(&PlanInput {
            record: Some(QuotaState {
                reset_at: Some("7am (UTC)".into()),
                reset_unix: Some(1),
                since: Utc::now(),
            }),
            ..input()
        });
        assert_eq!(row["exhausted"], false);
        assert_eq!(row["reset_unix"], Value::Null);
        assert_eq!(row["last_limit"]["reset_unix"], 1);
    }

    #[test]
    fn percent_left_needs_both_the_balance_and_the_limit() {
        let both = plan_json(&PlanInput {
            balance: Some(json!({"remaining": 2500, "limit": 10000, "error": null, "checked_at": "2026-10-05T10:00:00Z"})),
            ..input()
        });
        assert_eq!(both["balance"]["pct_left"], 25.0);
        assert_eq!(both["balance"]["checked_at"], "2026-10-05T10:00:00Z");
        let remaining_only = plan_json(&PlanInput {
            balance: Some(json!({"remaining": 2500, "error": null})),
            ..input()
        });
        assert_eq!(remaining_only["balance"]["remaining"], 2500);
        assert_eq!(remaining_only["balance"]["pct_left"], Value::Null);
        let failed = plan_json(&PlanInput {
            balance: Some(json!({"remaining": null, "limit": null, "error": "quota endpoint answered HTTP 404"})),
            ..input()
        });
        assert_eq!(failed["balance"]["pct_left"], Value::Null);
        assert_eq!(failed["balance"]["error"], "quota endpoint answered HTTP 404");
    }

    #[test]
    fn the_request_counts_come_from_the_gateway_usage() {
        let row = plan_json(&PlanInput {
            usage: Some(ProviderUsage {
                requests: 1240,
                failures: 3,
                ..Default::default()
            }),
            ..input()
        });
        assert_eq!(row["requests"], 1240);
        assert_eq!(row["failures"], 3);
        let claude = plan_json(&PlanInput {
            kind: "claude",
            ..input()
        });
        assert_eq!(
            claude["requests"],
            Value::Null,
            "the gateway does not proxy the Claude account"
        );
    }
}
