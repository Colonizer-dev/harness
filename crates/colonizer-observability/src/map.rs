//! Ledger lines to OTLP log records (#845, first slice): the harness log, agent events, gateway
//! requests, activity and the spend journal. Every value goes through the [`Policy`], so every
//! attribute is allowlisted for its source, every string is redacted again and capped, and the
//! content tier (prompts, completions, tool input and output, error text, paths, titles) stays
//! behind the content gate — which this slice never opens.

use crate::batch::Item;
use crate::contract::ColonyPolicy;
use crate::policy::{AttrValue, LogBuilder, Policy, Source, Tier};
use crate::proto::logs::v1::SeverityNumber;
use crate::sources::stream;
use serde_json::Value;
use std::collections::BTreeMap;

/// `colonizer.record.id` (docs/design/observability.md, Identity): the first 16 bytes of
/// `sha256("colonizer.rec.v1|" + host_id + "|" + source + "|" + colony_or_dash + "|" + key)`, as
/// 32 lowercase hex characters.
pub fn record_id(host_id: &str, source: Source, colony_or_dash: &str, key: &str) -> String {
    let input = format!("colonizer.rec.v1|{host_id}|{}|{colony_or_dash}|{key}", source.as_str());
    let digest = ring::digest::digest(&ring::digest::SHA256, input.as_bytes());
    hex(&digest.as_ref()[..16])
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A line's `ts` (RFC 3339) in Unix nanoseconds.
pub fn ts_nanos(line: &Value) -> Option<u64> {
    let ts = line.get("ts")?.as_str()?;
    let parsed = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    u64::try_from(parsed.timestamp_nanos_opt()?).ok()
}

/// A JSON scalar as an attribute value; objects, arrays and nulls are not attributes.
fn scalar(value: &Value) -> Option<AttrValue> {
    match value {
        Value::String(s) => Some(AttrValue::Str(s.clone())),
        Value::Bool(b) => Some(AttrValue::Bool(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Some(AttrValue::Int(i)),
            None => n.as_f64().map(AttrValue::Double),
        },
        _ => None,
    }
}

fn severity_of(level: &str) -> SeverityNumber {
    match level.to_ascii_lowercase().as_str() {
        "trace" => SeverityNumber::Trace,
        "debug" => SeverityNumber::Debug,
        "warn" | "warning" => SeverityNumber::Warn,
        "error" => SeverityNumber::Error,
        "fatal" => SeverityNumber::Fatal,
        _ => SeverityNumber::Info,
    }
}

/// Maps lines of one source to log records.
pub struct Mapper<'a> {
    pub policy: &'a Policy,
    pub host_id: &'a str,
    pub colonies: &'a BTreeMap<String, ColonyPolicy>,
    /// Stamped on a record whose line carries no `ts`.
    pub now_unix_nanos: u64,
}

/// The keys an agent event may carry as attributes, copied straight from the line when present.
const EVENT_KEYS: [&str; 12] = [
    "state",
    "risk",
    "kind",
    "blocking",
    "tool_call_id",
    "is_error",
    "question_id",
    "model",
    "cost_usd",
    "duration_ms",
    "access",
    "policy",
];

const GATEWAY_KEYS: [&str; 13] = [
    "provider",
    "wire",
    "model",
    "wire_model",
    "status",
    "failure",
    "fallback",
    "queue_ms",
    "duration_ms",
    "request_bytes",
    "response_bytes",
    "input_tokens",
    "output_tokens",
];

const ACTIVITY_KEYS: [&str; 5] = ["kind", "actor", "colony", "repo", "target"];

const SPEND_KEYS: [&str; 11] = [
    "kind",
    "org",
    "session",
    "agent",
    "model",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "cost_usd",
    "scoring_ms",
];

impl Mapper<'_> {
    /// One line of `source` as a log record, or `None` for a line this slice does not export
    /// (a streaming text delta). `colony` is the colony a per-colony file belongs to; `digest` is
    /// the SHA-256 of the line's raw bytes, the record id's key where the source has no `seq`.
    pub fn log(&self, source: Source, colony: Option<&str>, line: &Value, digest: &[u8; 32]) -> Option<Item> {
        let kind = line.get("type").and_then(Value::as_str).unwrap_or("");
        if source == Source::Events && kind == "assistant_text_delta" {
            return None;
        }
        // Only `events` and `activity` number their lines; their record id is keyed by `seq`.
        let key = match (source, line.get("seq").and_then(Value::as_u64)) {
            (Source::Events | Source::Activity, Some(seq)) => seq.to_string(),
            _ => hex(digest),
        };
        let colony_or_dash = colony.unwrap_or("-");
        let colony_id = match source {
            Source::Activity => line.get("colony").and_then(Value::as_str),
            Source::Spend => line.get("session").and_then(Value::as_str),
            _ => colony,
        };

        let mut log = self
            .policy
            .log(source)
            .time(ts_nanos(line).unwrap_or(self.now_unix_nanos))
            .attr(
                "colonizer.record.id",
                record_id(self.host_id, source, colony_or_dash, &key),
                Tier::Structure,
            )
            .attr("colonizer.source", source.as_str(), Tier::Structure)
            .attr("colonizer.stream", stream(source), Tier::Structure);
        if let Some(id) = colony_id {
            log = log.attr("colonizer.colony.id", id, Tier::Structure);
            if let Some(p) = self.colonies.get(id) {
                if !p.org.is_empty() {
                    log = log.attr("colonizer.org", p.org.as_str(), Tier::Structure);
                }
                if !p.repo.is_empty() {
                    log = log.attr("colonizer.repo", p.repo.as_str(), Tier::Structure);
                }
            }
        }
        let copy = |log, keys: &[&str]| copy_keys(log, line, keys);

        let log = match source {
            Source::Harness => {
                let level = line.get("level").and_then(Value::as_str).unwrap_or("info");
                let message = line.get("message").and_then(Value::as_str).unwrap_or("");
                copy(log, &["origin", "level"])
                    .severity(severity_of(level))
                    .event_name("colonizer.harness_log")
                    .body(message, Tier::Structure)
            }
            Source::Events => {
                let mut log = copy(log.attr("type", kind, Tier::Structure), &EVENT_KEYS);
                // A tool's name is structure; its input and output are content and never read here.
                if kind == "tool_call"
                    && let Some(name) = line.get("name").and_then(Value::as_str)
                {
                    log = log.attr("name", name, Tier::Structure);
                }
                if let Some(class) = line.pointer("/denial/class").and_then(Value::as_str) {
                    log = log.attr("denial.class", class, Tier::Structure);
                }
                for key in ["id", "name"] {
                    if let Some(v) = line.get("agent_ref").and_then(|r| r.get(key)).and_then(Value::as_str) {
                        log = log.attr(&format!("agent_ref.{key}"), v, Tier::Structure);
                    }
                }
                if let Some(Value::Object(usage)) = line.get("model_usage") {
                    for (model, tokens) in usage {
                        // The key carries the model's name, so only a name the allowlist's prefix
                        // rule accepts (short, name characters only) becomes one.
                        let name_ok = model.len() <= 40
                            && model
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
                        if !name_ok {
                            continue;
                        }
                        for field in ["input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens"] {
                            if let Some(n) = tokens.get(field).and_then(Value::as_i64) {
                                log = log.attr(&format!("model_usage.{model}.{field}"), n, Tier::Structure);
                            }
                        }
                    }
                }
                let errored = line.get("is_error").and_then(Value::as_bool) == Some(true);
                let severity = match kind {
                    "log" => severity_of(line.get("level").and_then(Value::as_str).unwrap_or("info")),
                    _ if errored => SeverityNumber::Warn,
                    _ => SeverityNumber::Info,
                };
                log.severity(severity)
                    .event_name("colonizer.agent_event")
                    .body(kind, Tier::Structure)
            }
            Source::Gateway => {
                let failed = line.get("failure").is_some_and(|f| !f.is_null());
                copy(log, &GATEWAY_KEYS)
                    .severity(if failed { SeverityNumber::Warn } else { SeverityNumber::Info })
                    .event_name("colonizer.gateway_request")
                    .body("gateway_request", Tier::Structure)
            }
            Source::Activity => {
                let kind = line.get("kind").and_then(Value::as_str).unwrap_or("");
                // `detail` is content for chat, question and answer lines and error text on the
                // rest: it is never copied here.
                copy(log, &ACTIVITY_KEYS)
                    .severity(if kind == "outcome.failed" {
                        SeverityNumber::Warn
                    } else {
                        SeverityNumber::Info
                    })
                    .event_name("colonizer.activity")
                    .body(kind, Tier::Structure)
            }
            Source::Spend => copy(log, &SPEND_KEYS)
                .severity(SeverityNumber::Info)
                .event_name("colonizer.spend")
                .body("spend", Tier::Structure),
            // Not mapped in this slice (#845 follow-ups); `sources` never lists them.
            _ => return None,
        };
        Some(log.finish())
    }
}

/// The scalar values of `keys` on `line`, copied as structure attributes.
fn copy_keys<'p>(mut log: LogBuilder<'p>, line: &Value, keys: &[&str]) -> LogBuilder<'p> {
    for key in keys {
        if let Some(v) = line.get(*key).and_then(scalar) {
            log = log.attr(key, v, Tier::Structure);
        }
    }
    log
}

#[cfg(test)]
mod tests;
