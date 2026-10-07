//! A provider's usage history (issue #1204): what the Model providers page draws as a credits
//! chart and a requests sparkline. The gateway only keeps cumulative counters
//! (`provider-usage.json`), so this module keeps the other half: per-day request, failure and
//! latency tallies (the day's share of the counters' growth), plan-balance samples, and the
//! plan's exhausted / recovered / reset events, all for 30 days, in `<data_dir>/provider-history.json`.
//!
//! Nothing here is estimated. A day with no traffic is zeros, a provider with no balance reader
//! has no balance samples, and the response says which of the two it is. The cockpit draws the
//! run-out projection from the samples; the Mothership never invents a reset time.

use crate::{ApiResult, Shared, client_error, gateway::ACCOUNT_QUOTA_ID, gateway::ProviderUsage};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// How long a day, a balance sample or an event is kept.
pub(crate) const KEEP_DAYS: i64 = 30;
/// Two samples closer than this with the same value are one reading: the Check button and the
/// hourly reader must not fill the chart with copies.
const MIN_SAMPLE_GAP_SECS: i64 = 60;
/// A hard cap on samples per provider, whatever the cadence, so the file stays small.
const MAX_SAMPLES: usize = 2000;
/// How often the background task samples the counters and persists changes.
const TICK: std::time::Duration = std::time::Duration::from_secs(30);
/// How often the background task reads each provider's plan balance.
const BALANCE_EVERY: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// One day's share of a provider's traffic.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DayTally {
    pub requests: u64,
    pub failures: u64,
    pub duration_ms: u64,
}

/// One reading of a plan's remaining balance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BalanceSample {
    pub at: DateTime<Utc>,
    pub remaining: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// When the plan refills, if the quota probe has a reset pointer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_unix: Option<i64>,
}

/// A plan state change: `exhausted`, `recovered`, or `reset` (the balance jumped back up).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Event {
    pub at: DateTime<Utc>,
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ProviderHistory {
    /// `YYYY-MM-DD` (UTC) to that day's tally.
    pub days: BTreeMap<String, DayTally>,
    pub balance: Vec<BalanceSample>,
    pub events: Vec<Event>,
    /// The cumulative counters at the last sample, so the next one counts only what grew.
    pub last: Option<DayTally>,
}

pub(crate) struct History {
    file: PathBuf,
    inner: Mutex<BTreeMap<String, ProviderHistory>>,
    dirty: AtomicBool,
}

fn day_key(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d").to_string()
}

impl History {
    /// Loads `<data_dir>/provider-history.json`; a missing or corrupt file starts the history over.
    pub(crate) fn load(data_dir: &std::path::Path) -> Self {
        let file = data_dir.join("provider-history.json");
        let saved = std::fs::read(&file)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self {
            file,
            inner: Mutex::new(saved),
            dirty: AtomicBool::new(false),
        }
    }

    fn with<R>(&self, provider: &str, f: impl FnOnce(&mut ProviderHistory) -> R) -> R {
        let mut map = self.inner.lock().unwrap();
        let out = f(map.entry(provider.to_string()).or_default());
        self.dirty.store(true, Ordering::SeqCst);
        out
    }

    /// Adds what the cumulative counters grew by since the last call to `now`'s day. The first call
    /// for a provider only sets the baseline: the counters' history before this module existed
    /// has no day to belong to. Counters that went down (a deleted and re-added provider) restart.
    pub(crate) fn sample_usage(&self, provider: &str, usage: &ProviderUsage, now: DateTime<Utc>) {
        let current = DayTally {
            requests: usage.requests,
            failures: usage.failures,
            duration_ms: usage.duration_ms,
        };
        self.with(provider, |h| {
            let delta = match &h.last {
                None => DayTally::default(),
                Some(last) if current.requests >= last.requests => DayTally {
                    requests: current.requests - last.requests,
                    failures: current.failures.saturating_sub(last.failures),
                    duration_ms: current.duration_ms.saturating_sub(last.duration_ms),
                },
                Some(_) => current.clone(),
            };
            h.last = Some(current);
            if delta.requests > 0 {
                let day = h.days.entry(day_key(now)).or_default();
                day.requests += delta.requests;
                day.failures += delta.failures;
                day.duration_ms += delta.duration_ms;
            }
            prune(h, now);
        });
    }

    /// Records a plan-balance reading, and a `reset` event when the balance jumped up by a tenth of
    /// the plan (or, with no total known, by half of the new value): a refilled plan.
    pub(crate) fn record_balance(
        &self,
        provider: &str,
        remaining: f64,
        limit: Option<f64>,
        reset_unix: Option<i64>,
        now: DateTime<Utc>,
    ) {
        if !remaining.is_finite() {
            return;
        }
        self.with(provider, |h| {
            if let Some(prev) = h.balance.last() {
                let jump = remaining - prev.remaining;
                let threshold = limit.or(prev.limit).map_or(remaining * 0.5, |l| l * 0.1);
                if jump > 0.0 && jump >= threshold {
                    h.events.push(Event {
                        at: now,
                        kind: "reset".into(),
                    });
                }
                if (now - prev.at).num_seconds() < MIN_SAMPLE_GAP_SECS && prev.remaining == remaining {
                    return;
                }
            }
            h.balance.push(BalanceSample {
                at: now,
                remaining,
                limit,
                reset_unix,
            });
            prune(h, now);
        });
    }

    /// Records a plan state change.
    pub(crate) fn record_event(&self, provider: &str, kind: &str, now: DateTime<Utc>) {
        self.with(provider, |h| {
            h.events.push(Event {
                at: now,
                kind: kind.into(),
            });
            prune(h, now);
        });
    }

    /// The latest balance reading, for the provider list.
    pub(crate) fn latest_balance(&self, provider: &str) -> Option<BalanceSample> {
        self.inner.lock().unwrap().get(provider)?.balance.last().cloned()
    }

    /// Forgets a deleted provider's history.
    pub(crate) fn forget(&self, provider: &str) {
        if self.inner.lock().unwrap().remove(provider).is_some() {
            self.dirty.store(true, Ordering::SeqCst);
            self.flush();
        }
    }

    /// Writes the file if anything changed since the last write.
    pub(crate) fn flush(&self) {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let snapshot = self.inner.lock().unwrap().clone();
        crate::gateway::write_json_atomic(&self.file, &snapshot);
    }

    /// The report `GET /api/providers/{id}/usage?days=` serves: one row per day for the last `days`
    /// days ending today (zero-filled, oldest first), the balance samples and the events in that window.
    pub(crate) fn report(&self, provider: &str, days: i64, now: DateTime<Utc>) -> Value {
        let days = days.clamp(1, KEEP_DAYS);
        let map = self.inner.lock().unwrap();
        let empty = ProviderHistory::default();
        let h = map.get(provider).unwrap_or(&empty);
        let today = now.date_naive();
        let since = now - Duration::days(days);
        let daily: Vec<Value> = (0..days)
            .map(|back| {
                let date: NaiveDate = today - Duration::days(days - 1 - back);
                let key = date.format("%Y-%m-%d").to_string();
                let tally = h.days.get(&key).cloned().unwrap_or_default();
                let avg = if tally.requests == 0 {
                    0
                } else {
                    tally.duration_ms / tally.requests
                };
                json!({
                    "date": key,
                    "requests": tally.requests,
                    "failures": tally.failures,
                    "avg_latency_ms": avg,
                })
            })
            .collect();
        let balance: Vec<&BalanceSample> = h.balance.iter().filter(|s| s.at >= since).collect();
        let resets: Vec<&Event> = h.events.iter().filter(|e| e.at >= since).collect();
        json!({
            "provider": provider,
            "days": days,
            "daily": daily,
            "balance": balance,
            "events": resets,
            "has_balance": !h.balance.is_empty(),
        })
    }
}

fn prune(h: &mut ProviderHistory, now: DateTime<Utc>) {
    let cutoff = now - Duration::days(KEEP_DAYS);
    let cutoff_day = day_key(cutoff);
    h.days.retain(|day, _| day.as_str() >= cutoff_day.as_str());
    h.balance.retain(|s| s.at >= cutoff);
    h.events.retain(|e| e.at >= cutoff);
    if h.balance.len() > MAX_SAMPLES {
        let extra = h.balance.len() - MAX_SAMPLES;
        h.balance.drain(..extra);
    }
}

#[derive(Deserialize)]
struct UsageQuery {
    days: Option<i64>,
}

/// `GET /api/providers/{id}/usage?days=` (default 7, at most 30). `anthropic` is the Claude
/// account: no gateway counters, but its limit events.
async fn usage(State(app): State<Shared>, Path(id): Path<String>, Query(q): Query<UsageQuery>) -> ApiResult<Value> {
    let key = if id == "anthropic" {
        ACCOUNT_QUOTA_ID.to_string()
    } else if app.providers().iter().any(|p| p.id == id) {
        id.clone()
    } else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such provider"));
    };
    let mut report = app.gateway.history.report(&key, q.days.unwrap_or(7), Utc::now());
    report["provider"] = json!(id);
    // The plan's current exhaustion, so the chart can mark the reset it is waiting for.
    if let Some(state) = app.gateway.quota_state(&key).filter(|_| app.gateway.is_quota_exhausted(&key)) {
        report["exhausted"] = json!({"reset_at": state.reset_at, "reset_unix": state.reset_unix});
    }
    Ok(Json(report))
}

pub(crate) fn routes() -> axum::Router<Shared> {
    axum::Router::new().route("/api/providers/{id}/usage", axum::routing::get(usage))
}

/// Samples every provider's counters each [`TICK`], reads plan balances each [`BALANCE_EVERY`], and
/// writes what changed.
fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut last_balance: Option<std::time::Instant> = None;
        loop {
            tokio::time::sleep(TICK).await;
            let now = Utc::now();
            let providers = app.providers();
            for p in &providers {
                app.gateway.history.sample_usage(&p.id, &app.gateway.usage(&p.id), now);
            }
            if last_balance.is_none_or(|t| t.elapsed() >= BALANCE_EVERY) {
                last_balance = Some(std::time::Instant::now());
                // Recorded by the probe itself; this only asks.
                futures_util::future::join_all(
                    providers
                        .iter()
                        .filter(|p| p.quota.is_some())
                        .map(|p| crate::gateway::probe_quota(&app, p)),
                )
                .await;
            }
            app.gateway.history.flush();
        }
    });
}

pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "provider_history",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: Some(start_tasks),
};

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    fn usage(requests: u64, failures: u64, duration_ms: u64) -> ProviderUsage {
        ProviderUsage {
            requests,
            failures,
            duration_ms,
            ..Default::default()
        }
    }
    fn history() -> History {
        History::load(&std::env::temp_dir().join(format!("colonizer-history-{}", crate::util::short_id())))
    }

    #[test]
    fn the_first_sample_is_a_baseline_and_later_ones_count_growth_per_day() {
        let h = history();
        h.sample_usage("p", &usage(100, 5, 50_000), at("2026-10-05T10:00:00Z"));
        h.sample_usage("p", &usage(110, 6, 60_000), at("2026-10-05T23:59:00Z"));
        h.sample_usage("p", &usage(130, 6, 80_000), at("2026-10-06T00:01:00Z"));
        let report = h.report("p", 3, at("2026-10-06T12:00:00Z"));
        let daily = report["daily"].as_array().unwrap();
        assert_eq!(daily.len(), 3);
        assert_eq!(daily[0]["date"], "2026-10-04");
        assert_eq!(daily[0]["requests"], 0, "a quiet day is zeros, not missing");
        assert_eq!(daily[1]["date"], "2026-10-05");
        assert_eq!(daily[1]["requests"], 10);
        assert_eq!(daily[1]["failures"], 1);
        assert_eq!(daily[1]["avg_latency_ms"], 1000);
        assert_eq!(daily[2]["requests"], 20);
        assert_eq!(daily[2]["avg_latency_ms"], 1000);
    }

    #[test]
    fn counters_that_restart_count_from_zero() {
        let h = history();
        h.sample_usage("p", &usage(100, 0, 0), at("2026-10-05T10:00:00Z"));
        h.sample_usage("p", &usage(4, 1, 400), at("2026-10-05T11:00:00Z"));
        let report = h.report("p", 1, at("2026-10-05T12:00:00Z"));
        assert_eq!(report["daily"][0]["requests"], 4);
    }

    #[test]
    fn balance_samples_dedupe_and_a_jump_up_is_a_reset() {
        let h = history();
        h.record_balance("p", 800.0, Some(1000.0), None, at("2026-10-05T10:00:00Z"));
        h.record_balance("p", 800.0, Some(1000.0), None, at("2026-10-05T10:00:30Z"));
        h.record_balance("p", 300.0, Some(1000.0), None, at("2026-10-05T14:00:00Z"));
        h.record_balance("p", 990.0, Some(1000.0), None, at("2026-10-06T00:00:00Z"));
        let report = h.report("p", 7, at("2026-10-06T01:00:00Z"));
        assert_eq!(
            report["balance"].as_array().unwrap().len(),
            3,
            "the repeat reading within a minute is dropped"
        );
        let events = report["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "reset");
        assert_eq!(report["has_balance"], true);
    }

    #[test]
    fn a_provider_with_no_reader_says_so_and_old_history_is_pruned() {
        let h = history();
        let report = h.report("never-read", 7, at("2026-10-06T01:00:00Z"));
        assert_eq!(report["has_balance"], false);
        assert_eq!(report["balance"].as_array().unwrap().len(), 0);
        h.record_balance("p", 5.0, None, None, at("2026-08-01T00:00:00Z"));
        h.record_event("p", "exhausted", at("2026-10-05T00:00:00Z"));
        let report = h.report("p", 30, at("2026-10-06T00:00:00Z"));
        assert_eq!(report["balance"].as_array().unwrap().len(), 0, "a 66-day-old sample is gone");
        assert_eq!(report["events"][0]["kind"], "exhausted");
        assert_eq!(
            h.report("p", 400, at("2026-10-06T00:00:00Z"))["days"],
            30,
            "days are capped at 30"
        );
    }

    #[test]
    fn a_reset_pointer_reads_unix_seconds_milliseconds_and_rfc3339() {
        use crate::gateway::quota_reset as reset;
        let body = json!({"s": 1_790_000_000, "ms": 1_790_000_000_000_i64, "iso": "2026-10-07T12:00:00Z", "q": "1790000000", "bad": "soon"});
        assert_eq!(reset(&body, "/s"), Some(1_790_000_000));
        assert_eq!(reset(&body, "/ms"), Some(1_790_000_000));
        assert_eq!(reset(&body, "/iso"), Some(1_791_374_400));
        assert_eq!(reset(&body, "/q"), Some(1_790_000_000));
        assert_eq!(reset(&body, "/bad"), None);
        assert_eq!(reset(&body, "/missing"), None);
    }

    #[test]
    fn history_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("colonizer-history-{}", crate::util::short_id()));
        let h = History::load(&dir);
        h.record_balance("p", 42.0, None, None, at("2026-10-05T10:00:00Z"));
        h.flush();
        let again = History::load(&dir);
        assert_eq!(again.latest_balance("p").map(|s| s.remaining), Some(42.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
