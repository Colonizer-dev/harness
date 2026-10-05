//! The metrics catalogue's first slice (#852/#853): cumulative series folded from the gateway and
//! spend ledgers, gauges from the colonies the mothership lists in the contract, and the exporter's
//! own counters. The folded series live in `state.json` next to the cursors that produced them, and
//! are committed in the same atomic write, so a restart neither loses nor double-counts a line.
//!
//! | Metric | Kind | Unit | Attributes |
//! | :--- | :--- | :--- | :--- |
//! | `colonizer.colonies` | gauge | `{colony}` | `colonizer.colony.status` |
//! | `colonizer.queue.depth` | gauge | `{colony}` | — |
//! | `colonizer.gateway.requests` | counter | `{request}` | `provider`, `model` |
//! | `colonizer.gateway.failures` | counter | `{request}` | `provider`, `model`, `failure` |
//! | `colonizer.gateway.duration` | histogram | `ms` | `provider`, `model` |
//! | `colonizer.gateway.queue_time` | histogram | `ms` | `provider`, `model` |
//! | `colonizer.gateway.input_tokens`, `.output_tokens` | counter | `{token}` | `provider`, `model` |
//! | `colonizer.spend.cost` | counter | `USD` | `org`, `model`, `kind` |
//! | `colonizer.spend.input_tokens`, `.output_tokens` | counter | `{token}` | `org`, `model`, `kind` |
//! | `colonizer.observability.exported` | counter | `{record}` | — |
//! | `colonizer.observability.dropped` | counter | `{record}` | `colonizer.drop.reason` |
//! | `colonizer.observability.export_failures` | counter | `{request}` | — |

use crate::batch::Item;
use crate::contract::ColonyPolicy;
use crate::policy::{Policy, Source, Tier};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Bucket bounds for the latency histograms, in milliseconds.
pub const LATENCY_BOUNDS_MS: [f64; 12] = [
    50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10_000.0, 30_000.0, 60_000.0, 120_000.0, 300_000.0,
];
/// The most series one family keeps; a line that would open another is counted as dropped instead,
/// so a misbehaving provider id cannot grow memory or a backend's cardinality without bound.
pub const MAX_SERIES: usize = 512;

/// One explicit-bucket histogram.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Histogram {
    pub counts: Vec<u64>,
    pub sum: f64,
}

impl Histogram {
    fn observe(&mut self, value: f64) {
        if self.counts.len() != LATENCY_BOUNDS_MS.len() + 1 {
            self.counts = vec![0; LATENCY_BOUNDS_MS.len() + 1];
        }
        let bucket = LATENCY_BOUNDS_MS
            .iter()
            .position(|b| value <= *b)
            .unwrap_or(LATENCY_BOUNDS_MS.len());
        self.counts[bucket] += 1;
        self.sum += value;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GatewaySeries {
    pub provider: String,
    pub model: String,
    pub requests: u64,
    pub failures: BTreeMap<String, u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration: Histogram,
    pub queue: Histogram,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendSeries {
    pub org: String,
    pub model: String,
    pub kind: String,
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Every cumulative series, as committed in `state.json` under `extra.metrics`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Aggregates {
    /// When these sums started: a reset (new state) starts a new cumulative stream.
    pub start_unix_nanos: u64,
    pub gateway: BTreeMap<String, GatewaySeries>,
    pub spend: BTreeMap<String, SpendSeries>,
    pub exported: u64,
    pub dropped: BTreeMap<String, u64>,
    pub export_failures: u64,
}

fn str_at<'a>(line: &'a Value, key: &str) -> &'a str {
    line.get(key).and_then(Value::as_str).unwrap_or("")
}

impl Aggregates {
    pub fn drop_count(&mut self, reason: &str, n: u64) {
        if n > 0 {
            *self.dropped.entry(reason.to_string()).or_default() += n;
        }
    }

    /// Folds one ledger line of `source` into the series.
    pub fn fold(&mut self, source: Source, line: &Value) {
        match source {
            Source::Gateway => self.fold_gateway(line),
            Source::Spend => self.fold_spend(line),
            _ => {}
        }
    }

    fn fold_gateway(&mut self, line: &Value) {
        let provider = str_at(line, "provider");
        let model = str_at(line, "model");
        let key = format!("{provider}\u{1f}{model}");
        if !self.gateway.contains_key(&key) && self.gateway.len() >= MAX_SERIES {
            self.drop_count("series_limit", 1);
            return;
        }
        let s = self.gateway.entry(key).or_insert_with(|| GatewaySeries {
            provider: provider.to_string(),
            model: model.to_string(),
            ..GatewaySeries::default()
        });
        s.requests += 1;
        if let Some(failure) = line.get("failure").and_then(Value::as_str) {
            *s.failures.entry(failure.to_string()).or_default() += 1;
        }
        s.input_tokens += line.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
        s.output_tokens += line.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
        if let Some(ms) = line.get("duration_ms").and_then(Value::as_f64) {
            s.duration.observe(ms);
        }
        if let Some(ms) = line.get("queue_ms").and_then(Value::as_f64) {
            s.queue.observe(ms);
        }
    }

    fn fold_spend(&mut self, line: &Value) {
        let (org, model, kind) = (str_at(line, "org"), str_at(line, "model"), str_at(line, "kind"));
        let key = format!("{org}\u{1f}{model}\u{1f}{kind}");
        if !self.spend.contains_key(&key) && self.spend.len() >= MAX_SERIES {
            self.drop_count("series_limit", 1);
            return;
        }
        let s = self.spend.entry(key).or_insert_with(|| SpendSeries {
            org: org.to_string(),
            model: model.to_string(),
            kind: kind.to_string(),
            ..SpendSeries::default()
        });
        s.cost_usd += line.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
        s.input_tokens += line.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
        s.output_tokens += line.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
    }

    /// Every metric point, at `now`. Values are cumulative since `start_unix_nanos`; the colony
    /// gauges are read from the contract's colony list as it is now.
    pub fn points(&self, policy: &Policy, colonies: &BTreeMap<String, ColonyPolicy>, now: u64) -> Vec<Item> {
        let start = self.start_unix_nanos;
        let mut out = Vec::new();

        let mut by_status: BTreeMap<&str, i64> = BTreeMap::new();
        for p in colonies.values() {
            *by_status.entry(p.status.as_deref().unwrap_or("unknown")).or_default() += 1;
        }
        for (status, n) in &by_status {
            out.push(
                policy
                    .metric(Source::Mothership, "colonizer.colonies", "{colony}")
                    .times(start, now)
                    .int(*n)
                    .attr("colonizer.colony.status", *status, Tier::Structure)
                    .finish(),
            );
        }
        out.push(
            policy
                .metric(Source::Mothership, "colonizer.queue.depth", "{colony}")
                .times(start, now)
                .int(by_status.get("queued").copied().unwrap_or(0))
                .finish(),
        );

        for s in self.gateway.values() {
            let base = |name: &'static str, unit: &'static str| {
                policy
                    .metric(Source::Gateway, name, unit)
                    .times(start, now)
                    .attr("provider", s.provider.as_str(), Tier::Structure)
                    .attr("model", s.model.as_str(), Tier::Structure)
            };
            out.push(
                base("colonizer.gateway.requests", "{request}")
                    .sum(true)
                    .int(s.requests as i64)
                    .finish(),
            );
            for (failure, n) in &s.failures {
                out.push(
                    base("colonizer.gateway.failures", "{request}")
                        .sum(true)
                        .int(*n as i64)
                        .attr("failure", failure.as_str(), Tier::Structure)
                        .finish(),
                );
            }
            out.push(
                base("colonizer.gateway.input_tokens", "{token}")
                    .sum(true)
                    .int(s.input_tokens as i64)
                    .finish(),
            );
            out.push(
                base("colonizer.gateway.output_tokens", "{token}")
                    .sum(true)
                    .int(s.output_tokens as i64)
                    .finish(),
            );
            if !s.duration.counts.is_empty() {
                out.push(
                    base("colonizer.gateway.duration", "ms")
                        .histogram(&LATENCY_BOUNDS_MS, &s.duration.counts, s.duration.sum)
                        .finish(),
                );
            }
            if !s.queue.counts.is_empty() {
                out.push(
                    base("colonizer.gateway.queue_time", "ms")
                        .histogram(&LATENCY_BOUNDS_MS, &s.queue.counts, s.queue.sum)
                        .finish(),
                );
            }
        }

        for s in self.spend.values() {
            let base = |name: &'static str, unit: &'static str| {
                policy
                    .metric(Source::Spend, name, unit)
                    .times(start, now)
                    .sum(true)
                    .attr("org", s.org.as_str(), Tier::Structure)
                    .attr("model", s.model.as_str(), Tier::Structure)
                    .attr("kind", s.kind.as_str(), Tier::Structure)
            };
            out.push(base("colonizer.spend.cost", "USD").double(s.cost_usd).finish());
            out.push(
                base("colonizer.spend.input_tokens", "{token}")
                    .int(s.input_tokens as i64)
                    .finish(),
            );
            out.push(
                base("colonizer.spend.output_tokens", "{token}")
                    .int(s.output_tokens as i64)
                    .finish(),
            );
        }

        let own =
            |name: &'static str, unit: &'static str| policy.metric(Source::Mothership, name, unit).times(start, now).sum(true);
        out.push(
            own("colonizer.observability.exported", "{record}")
                .int(self.exported as i64)
                .finish(),
        );
        for (reason, n) in &self.dropped {
            out.push(
                own("colonizer.observability.dropped", "{record}")
                    .int(*n as i64)
                    .attr("colonizer.drop.reason", reason.as_str(), Tier::Structure)
                    .finish(),
            );
        }
        out.push(
            own("colonizer.observability.export_failures", "{request}")
                .int(self.export_failures as i64)
                .finish(),
        );
        out
    }
}
