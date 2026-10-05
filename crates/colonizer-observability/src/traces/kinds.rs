//! The spans that make a colony's trace useful to operate it (#847): who answered which question,
//! what the host chain decided, and which routed model calls the gateway made. Structure only, like
//! the rest of the trace: a question's risk and kind but never its text or the answer, a path
//! policy's decision but never the path, a verification's verdict but never its command or files.

use super::{Builder, Close, ColonyTrace, Open, Ref, SpanKindName, name_like, root_ref};
use crate::batch::Item;
use crate::map::{hex, ts_nanos};
use crate::policy::AttrValue;
use crate::proto::trace::v1::status::StatusCode;
use serde_json::Value;

/// The host chain's event types (`validation.rs` `emit_chain`, and the runner's `path_policy` and
/// `jev_ladder` reports): each becomes one `host_step <type>` span under the root.
pub(super) const HOST_CHAIN: [&str; 11] = [
    "validated",
    "rejected",
    "fix_colony",
    "review",
    "merged",
    "verification",
    "screening",
    "watchdog_turn_end",
    "boundary",
    "path_policy",
    "jev_ladder",
];

/// A committed scalar as an attribute value; anything else is not one.
pub(super) fn attr_value(value: &Value) -> Option<AttrValue> {
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

/// `line[key]` as an attribute under `attr`, when it is a name (a closed vocabulary value), a
/// number or a boolean. Free text never passes.
fn copy(attrs: &mut Vec<(String, Value)>, line: &Value, key: &str, attr: &str) {
    let value = match line.get(key) {
        Some(Value::String(s)) if name_like(s) => Value::String(s.clone()),
        Some(v @ (Value::Bool(_) | Value::Number(_))) => v.clone(),
        _ => return,
    };
    attrs.push((attr.to_string(), value));
}

/// The number of entries of an array field: a count, never the entries.
fn count(attrs: &mut Vec<(String, Value)>, line: &Value, key: &str, attr: &str) {
    if let Some(Value::Array(items)) = line.get(key) {
        attrs.push((attr.to_string(), Value::from(items.len())));
    }
}

impl Builder<'_> {
    /// A question opens a span under its turn (or its subagent), with its risk, kind, blocking flag
    /// and option count; the question and option text stay behind.
    pub(super) fn question(
        &self,
        colony: &str,
        trace: &mut ColonyTrace,
        line: &Value,
        agent: Option<&str>,
        ts: u64,
        out: &mut Vec<Item>,
    ) {
        let Some(id) = line.get("question_id").and_then(Value::as_str) else {
            return;
        };
        if trace.find(SpanKindName::Question, id).is_some() {
            return;
        }
        let parent = Self::parent(trace, colony, agent);
        let mut open = Open::new(Ref::new(SpanKindName::Question, id), parent, "", ts);
        // An absent risk is `workspace_write` (docs/agent-events.schema.json).
        let risk = line.get("risk").and_then(Value::as_str).unwrap_or("workspace_write");
        if name_like(risk) {
            open.attrs.push(("colonizer.question.risk".into(), Value::from(risk)));
        }
        copy(&mut open.attrs, line, "kind", "colonizer.question.kind");
        copy(&mut open.attrs, line, "blocking", "colonizer.question.blocking");
        if let Some(Value::Array(questions)) = line.get("questions") {
            let options: usize = questions
                .iter()
                .filter_map(|q| q.get("options").and_then(Value::as_array))
                .map(Vec::len)
                .sum();
            open.attrs.push(("colonizer.question.options".into(), Value::from(options)));
        }
        self.push_open(colony, trace, open, out);
    }

    /// An answer closes its question, saying whether a person or the autonomy judge gave it.
    pub(super) fn answered(&self, colony: &str, trace: &mut ColonyTrace, line: &Value, ts: u64, out: &mut Vec<Item>) {
        let Some(id) = line.get("question_id").and_then(Value::as_str) else {
            return;
        };
        let by = match line.get("origin").and_then(Value::as_str) {
            Some("autonomy") => "autonomy",
            _ => "user",
        };
        let close = Close {
            extra: vec![("colonizer.answered_by", AttrValue::Str(by.into()))],
            ..Close::default()
        };
        self.close(colony, trace, SpanKindName::Question, id, ts, close, out);
    }

    /// One host-chain verdict as a `host_step <type>` span under the root, keyed by the line's
    /// `seq` (or its digest): instantaneous, except a verification, which spans its run.
    pub(super) fn host_step(
        &self,
        colony: &str,
        trace: &mut ColonyTrace,
        line: &Value,
        digest: &[u8; 32],
        ts: u64,
        out: &mut Vec<Item>,
    ) {
        let kind = line.get("type").and_then(Value::as_str).unwrap_or("");
        let key = match line.get("seq").and_then(Value::as_u64) {
            Some(seq) => seq.to_string(),
            None => hex(digest),
        };
        let mut start = ts;
        let mut attrs = vec![("colonizer.step".to_string(), Value::from(kind))];
        let a = &mut attrs;
        match kind {
            "verification" => {
                copy(a, line, "verdict", "colonizer.verdict");
                copy(a, line, "by_declaration", "colonizer.verify.by_declaration");
                copy(a, line, "exit_code", "colonizer.verify.exit_code");
                copy(a, line, "commits", "colonizer.verify.commits");
                count(a, line, "files_changed", "colonizer.verify.files_changed");
                if let Some(ms) = line.get("ms").and_then(Value::as_u64) {
                    start = ts.saturating_sub(ms.saturating_mul(1_000_000));
                }
            }
            "screening" => {
                copy(a, line, "mode", "colonizer.screening.mode");
                copy(a, line, "outcome", "colonizer.screening.outcome");
                count(a, line, "findings", "colonizer.screening.findings");
            }
            "validated" => copy(a, line, "severity", "colonizer.finding.severity"),
            "review" => copy(a, line, "verdict", "colonizer.review.verdict"),
            "fix_colony" => copy(a, line, "session", "colonizer.fix.colony"),
            "watchdog_turn_end" => copy(a, line, "after_secs", "colonizer.watchdog.after_secs"),
            "boundary" => {
                copy(a, line, "kind", "colonizer.boundary.kind");
                copy(a, line, "control", "colonizer.boundary.control");
            }
            // The decision, never the path: paths are content.
            "path_policy" => {
                copy(a, line, "access", "colonizer.path_policy.access");
                copy(a, line, "policy", "colonizer.path_policy.policy");
                copy(a, line, "tool", "colonizer.path_policy.tool");
            }
            "jev_ladder" => {
                copy(a, line, "applied", "colonizer.jev.applied");
                copy(a, line, "pre_tokens", "colonizer.jev.pre_tokens");
                copy(a, line, "post_tokens", "colonizer.jev.post_tokens");
                copy(a, line, "trigger", "colonizer.jev.trigger");
                let decisions = line.get("decisions").and_then(Value::as_array);
                for (action, attr) in [
                    ("keep", "colonizer.jev.kept"),
                    ("drop_result", "colonizer.jev.dropped_results"),
                    ("drop_call", "colonizer.jev.dropped_calls"),
                ] {
                    let n = decisions.map_or(0, |d| {
                        d.iter()
                            .filter(|x| x.get("action").and_then(Value::as_str) == Some(action))
                            .count()
                    });
                    a.push((attr.to_string(), Value::from(n)));
                }
            }
            _ => {}
        }
        let mut open = Open::new(Ref::new(SpanKindName::HostStep, key), root_ref(colony), kind, start);
        open.attrs = attrs;
        let contradicted = kind == "verification" && line.get("verdict").and_then(Value::as_str) == Some("contradicted");
        let close = Close {
            status: contradicted.then_some(StatusCode::Error),
            error_type: contradicted.then_some("contradicted"),
            ..Close::default()
        };
        self.emit(colony, trace, &open, ts.max(start), &close, out);
    }

    /// One gateway request as a `chat <model>` client span under the root (the gateway is tailed
    /// apart from the events, so it has no turn), from when it was accepted to its last byte.
    pub(super) fn gateway(&self, colony: &str, trace: &mut ColonyTrace, line: &Value, digest: &[u8; 32], out: &mut Vec<Item>) {
        let Some(start) = ts_nanos(line) else { return };
        let duration = line.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
        let end = start.saturating_add(duration.saturating_mul(1_000_000));
        let name = |key: &str| line.get(key).and_then(Value::as_str).filter(|m| name_like(m));
        let subject = name("wire_model").or_else(|| name("model")).unwrap_or("");
        let mut open = Open::new(Ref::new(SpanKindName::Chat, hex(digest)), root_ref(colony), subject, start);
        let a = &mut open.attrs;
        a.push(("gen_ai.operation.name".into(), Value::from("chat")));
        copy(a, line, "provider", "gen_ai.provider.name");
        copy(a, line, "model", "gen_ai.request.model");
        copy(a, line, "wire_model", "gen_ai.response.model");
        copy(a, line, "input_tokens", "gen_ai.usage.input_tokens");
        copy(a, line, "output_tokens", "gen_ai.usage.output_tokens");
        copy(a, line, "wire", "colonizer.gateway.wire");
        copy(a, line, "status", "colonizer.gateway.status");
        copy(a, line, "queue_ms", "colonizer.gateway.queue_ms");
        copy(a, line, "fallback", "colonizer.fallback");
        let failure = line.get("failure").and_then(Value::as_str).filter(|f| name_like(f));
        let mut close = Close::default();
        if let Some(code) = failure {
            close.status = Some(StatusCode::Error);
            close.extra.push(("error.type", AttrValue::Str(code.to_string())));
        }
        self.emit(colony, trace, &open, end, &close, out);
    }
}
