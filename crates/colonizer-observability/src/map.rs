//! Ledger lines to OTLP log records (#845): every source but traces — the harness log, agent
//! events, gateway requests, activity, the spend journal, findings, decisions, routing, the Jev
//! ledgers, the mothership log and the exporter's own gap records. Every value goes through the
//! [`Policy`], so every attribute is allowlisted for its source, every string is redacted again and
//! capped, and the content tier (prompts, completions, tool input and output, error text, paths,
//! titles, free-text reasons) stays behind the content gate, which this build never opens.
//!
//! Names (docs/design/observability.md, "Log record names"): the event name is `colonizer.<what>`
//! in snake case, and a line's own fields keep their ledger names as attribute keys.

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

const ACTIVITY_KEYS: [&str; 6] = ["kind", "actor", "via", "colony", "repo", "target"];

/// The activity kinds whose `detail` was reviewed and is harness-authored structure, sent as
/// `summary`: a decision point's pick or miss (`decide.rs`), and the fixed suspension notice
/// (`queue.rs`). Every other
/// kind's `detail` (chat, questions, answers, error text, reasons, paths, and any kind added
/// later) is content tier, behind the gate.
pub const STRUCTURE_DETAIL_KINDS: [&str; 4] = ["decision.shadow", "decision.act", "decision.fallback", "outcome.suspended"];

/// `findings.jsonl`: the finding's stage and links; its title and reason are content.
const FINDING_KEYS: [&str; 8] = [
    "state",
    "issue",
    "duplicate_of",
    "severity",
    "verdict",
    "fix_session",
    "review_session",
    "pr",
];

/// `decisions.jsonl` (`decide::Row`); `options` is sent as a count, `outcome` as its scalars,
/// `did` from its closed vocabulary.
const DECISION_KEYS: [&str; 7] = ["kind", "point", "mode", "pick", "confidence", "latency_ms", "miss"];

/// `decide::Row::did`'s words. The redactor reads a key ending in `id` as an identifier and skips
/// its entropy layer, so `did` is never copied as free text: anything else is sent as `other`.
const DID_WORDS: [&str; 3] = ["jev", "rule", "cap"];

/// The scalars of a `routing.jsonl` `decision` row's record (boot.rs), sent as `decision.<key>`.
const ROUTING_DECISION_KEYS: [&str; 12] = [
    "point",
    "jev_mode",
    "jev_agrees",
    "floor",
    "tier",
    "rule",
    "source",
    "score",
    "model",
    "agent",
    "misroute",
    "sensitivity",
];

const JEV_LADDER_KEYS: [&str; 7] = [
    "kind",
    "tool",
    "tool_call_id",
    "action",
    "keep_call",
    "keep_result",
    "matched_tool_call_id",
];

/// `jev_focus.jsonl` (`verify_focus::FocusRow`); `candidates` is sent as a count.
const JEV_FOCUS_KEYS: [&str; 10] = [
    "kind",
    "session",
    "mode",
    "chosen",
    "would_catch",
    "verdict",
    "actual_first_failure_ms",
    "focused_first_failure_ms",
    "total_ms",
    "checks_run",
];

const EXPORT_GAP_KEYS: [&str; 5] = ["reason", "file", "bytes", "lines", "archived"];

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
        // The install-wide decision and Jev ledgers name their colony in the row's `session`; the
        // record id keys on it (docs/design/observability.md, Source inventory).
        let row_session = line.get("session").and_then(Value::as_str).filter(|s| !s.is_empty());
        let per_row = matches!(
            source,
            Source::Decisions | Source::Routing | Source::JevLadder | Source::JevFocus
        );
        let colony_or_dash = if per_row {
            row_session.unwrap_or("-")
        } else {
            colony.unwrap_or("-")
        };
        let colony_id = match source {
            Source::Activity => line.get("colony").and_then(Value::as_str),
            Source::Spend => row_session,
            _ if per_row => row_session,
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
                let mut log = copy(log, &ACTIVITY_KEYS);
                // `detail` is structure only for the reviewed kinds, sent as `summary`; for every
                // other kind it is content (`detail`), which the closed gate drops.
                if let Some(detail) = line.get("detail").and_then(Value::as_str) {
                    if STRUCTURE_DETAIL_KINDS.contains(&kind) {
                        log = log.attr("summary", detail, Tier::Structure);
                    } else {
                        log = log.attr("detail", detail, Tier::Content);
                    }
                }
                log
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
            Source::Findings => {
                let state = line.get("state").and_then(Value::as_str).unwrap_or("finding");
                let mut log = copy(log, &FINDING_KEYS);
                for key in ["title", "reason"] {
                    if let Some(text) = line.get(key).and_then(Value::as_str) {
                        log = log.attr(key, text, Tier::Content);
                    }
                }
                log.severity(SeverityNumber::Info)
                    .event_name("colonizer.finding")
                    .body(state, Tier::Structure)
            }
            Source::Decisions => {
                let mut log = copy(log, &DECISION_KEYS);
                if let Some(did) = line.get("did").and_then(Value::as_str) {
                    let word = DID_WORDS.iter().find(|w| **w == did).copied().unwrap_or("other");
                    log = log.attr("did", word, Tier::Structure);
                }
                if let Some(Value::Array(options)) = line.get("options") {
                    log = log.attr("options", options.len() as i64, Tier::Structure);
                }
                if let Some(Value::Object(outcome)) = line.get("outcome") {
                    for (key, value) in outcome {
                        if let Some(v) = scalar(value) {
                            log = log.attr(&format!("outcome.{key}"), v, Tier::Structure);
                        }
                    }
                }
                let point = line.get("point").and_then(Value::as_str).unwrap_or("decision");
                log.severity(SeverityNumber::Info)
                    .event_name("colonizer.decision")
                    .body(point, Tier::Structure)
            }
            Source::Routing => {
                let kind = line.get("kind").and_then(Value::as_str).unwrap_or("");
                let mut log = copy(log, &["kind", "actual_cost_usd"]);
                if let Some(record) = line.get("decision") {
                    for key in ROUTING_DECISION_KEYS {
                        if let Some(v) = record.get(key).and_then(scalar) {
                            log = log.attr(&format!("decision.{key}"), v, Tier::Structure);
                        }
                    }
                    // The rule's explanation is free text.
                    if let Some(reason) = record.get("reason").and_then(Value::as_str) {
                        log = log.attr("decision.reason", reason, Tier::Content);
                    }
                }
                log.severity(SeverityNumber::Info)
                    .event_name("colonizer.routing")
                    .body(kind, Tier::Structure)
            }
            Source::JevLadder => {
                let kind = line.get("kind").and_then(Value::as_str).unwrap_or("");
                copy(log, &JEV_LADDER_KEYS)
                    .severity(SeverityNumber::Info)
                    .event_name("colonizer.jev_ladder")
                    .body(kind, Tier::Structure)
            }
            Source::JevFocus => {
                let kind = line.get("kind").and_then(Value::as_str).unwrap_or("");
                let mut log = copy(log, &JEV_FOCUS_KEYS);
                if let Some(Value::Array(candidates)) = line.get("candidates") {
                    log = log.attr("candidates", candidates.len() as i64, Tier::Structure);
                }
                log.severity(SeverityNumber::Info)
                    .event_name("colonizer.jev_focus")
                    .body(kind, Tier::Structure)
            }
            Source::Mothership => {
                let level = line.get("level").and_then(Value::as_str).unwrap_or("info");
                let message = line.get("message").and_then(Value::as_str).unwrap_or("");
                copy(log, &["level", "target"])
                    .severity(severity_of(level))
                    .event_name("colonizer.mothership_log")
                    .body(message, Tier::Structure)
            }
            Source::ExportGap => {
                let reason = line.get("reason").and_then(Value::as_str).unwrap_or("");
                copy(log, &EXPORT_GAP_KEYS)
                    .severity(SeverityNumber::Warn)
                    .event_name("colonizer.export_gap")
                    .body(reason, Tier::Structure)
            }
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
