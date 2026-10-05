//! The trace state and ids (#846): what one colony's trace holds between ticks — its open spans,
//! turn counter and running totals, committed to `state.json` with the cursors — and the id and
//! sampling formulas of docs/design/observability.md, Identity.

use crate::policy::SpanKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The key, in `state.json`'s `extra`, the trace state is committed under (per destination).
pub(crate) const STATE_KEY: &str = "traces";

/// The trace id of `colony` on `host_id`.
pub fn trace_id(host_id: &str, colony: &str) -> [u8; 16] {
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        format!("colonizer.trace.v1|{host_id}|{colony}").as_bytes(),
    );
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest.as_ref()[..16]);
    id
}

/// The id of the span of `kind` keyed `key` in `trace`: the raw trace id, the kind and the key,
/// concatenated. The kinds are prefix-free, so no two (kind, key) pairs hash the same bytes.
pub fn span_id(trace: [u8; 16], kind: SpanKind, key: &str) -> [u8; 8] {
    let mut input = Vec::with_capacity(16 + 16 + key.len());
    input.extend_from_slice(&trace);
    input.extend_from_slice(kind.as_str().as_bytes());
    input.extend_from_slice(key.as_bytes());
    let digest = ring::digest::digest(&ring::digest::SHA256, &input);
    let mut id = [0u8; 8];
    id.copy_from_slice(&digest.as_ref()[..8]);
    id
}

/// Whether a colony's trace is exported: the trace id's first 8 bytes, as a big-endian u64, below
/// `ratio` of the range. Deterministic, so every replay and every machine agrees.
pub fn sampled(trace: [u8; 16], ratio: f64) -> bool {
    if ratio >= 1.0 {
        return true;
    }
    if ratio.is_nan() || ratio <= 0.0 {
        return false;
    }
    let mut head = [0u8; 8];
    head.copy_from_slice(&trace[..8]);
    (u64::from_be_bytes(head) as f64) < ratio * u64::MAX as f64
}

/// Whether `s` reads as a name (a tool, a subagent type, a model, an agent module) rather than free
/// text: a subagent's `name` is its task description when it has no type, and that is content.
pub(crate) fn name_like(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'/'))
}

/// Cumulative tokens, summed over every model of a `turn_end`'s `model_usage`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub(crate) fn of(usage: Option<&Value>) -> Option<Tokens> {
        let Value::Object(models) = usage? else { return None };
        let mut t = Tokens::default();
        for tokens in models.values() {
            let n = |k: &str| tokens.get(k).and_then(Value::as_u64).unwrap_or(0);
            t.input = t.input.saturating_add(n("input_tokens"));
            t.output = t.output.saturating_add(n("output_tokens"));
            t.cache_read = t.cache_read.saturating_add(n("cache_read_tokens"));
            t.cache_write = t.cache_write.saturating_add(n("cache_write_tokens"));
        }
        Some(t)
    }

    /// What one turn added over the last cumulative, floored at zero (a cheaper re-estimate never
    /// makes a negative turn).
    pub(crate) fn saturating_sub(self, before: Tokens) -> Tokens {
        Tokens {
            input: self.input.saturating_sub(before.input),
            output: self.output.saturating_sub(before.output),
            cache_read: self.cache_read.saturating_sub(before.cache_read),
            cache_write: self.cache_write.saturating_sub(before.cache_write),
        }
    }
}

/// A span's place in the tree, as the (kind, key) its id is computed from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Ref {
    pub kind: SpanKindName,
    pub key: String,
}

/// [`SpanKind`] as it is committed in `state.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpanKindName {
    InvokeAgent,
    Turn,
    Subagent,
    ExecuteTool,
    Chat,
    Question,
    HostStep,
}

impl SpanKindName {
    pub(crate) fn kind(self) -> SpanKind {
        match self {
            SpanKindName::InvokeAgent => SpanKind::InvokeAgent,
            SpanKindName::Turn => SpanKind::Turn,
            SpanKindName::Subagent => SpanKind::Subagent,
            SpanKindName::ExecuteTool => SpanKind::ExecuteTool,
            SpanKindName::Chat => SpanKind::Chat,
            SpanKindName::Question => SpanKind::Question,
            SpanKindName::HostStep => SpanKind::HostStep,
        }
    }
}

/// A span that has started and not ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Open {
    pub at: Ref,
    pub parent: Ref,
    /// The subject of its name: the tool, the subagent type, the turn number.
    pub subject: String,
    pub start: u64,
    /// A turn's `origin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// A subagent left running in the background (`tool_result.background`): its turn's end does
    /// not close it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
}

/// The root span's pending state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Root {
    /// Sent: never sent again, whatever comes later.
    pub emitted: bool,
    /// A final outcome seen, waiting for the colony's events to be read to their end.
    pub outcome: Option<String>,
    pub outcome_ts: u64,
    /// Suspensions, restores and stops: `(colonizer.<what>, ts)`.
    pub events: Vec<(String, u64)>,
}

/// One colony's trace state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ColonyTrace {
    /// Turns started so far: the current (or last) turn's number.
    pub turn: u64,
    /// Open spans, oldest first; the open turn, if any, among them.
    pub open: Vec<Open>,
    /// Cumulative tokens and cost as of the last `turn_end`.
    pub tokens: Tokens,
    pub cost_usd: f64,
    /// The orchestrator's model, from the last `model_changed`.
    pub model: Option<String>,
    pub first_ts: u64,
    pub last_ts: u64,
    pub root: Root,
    /// Subagents whose span has ended, so a late line never reopens one.
    pub ended_agents: BTreeSet<String>,
}

impl ColonyTrace {
    pub(crate) fn find(&self, kind: SpanKindName, key: &str) -> Option<usize> {
        self.open.iter().position(|o| o.at.kind == kind && o.at.key == key)
    }

    pub(crate) fn turn_open(&self) -> bool {
        self.open.iter().any(|o| o.at.kind == SpanKindName::Turn)
    }
}

/// Every traced colony's state, committed with the cursors.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Traces {
    pub colonies: BTreeMap<String, ColonyTrace>,
}

impl Traces {
    /// The state committed for `destination` in `extra`, or an empty one.
    pub(crate) fn load(extra: &BTreeMap<String, Value>, destination: &str) -> Traces {
        extra
            .get(STATE_KEY)
            .and_then(|v| v.get(destination))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Writes this state for `destination` into `extra`; other destinations' states are dropped,
    /// since their cursors are not read either.
    pub(crate) fn store(&self, extra: &mut BTreeMap<String, Value>, destination: &str) {
        let mut map = serde_json::Map::new();
        map.insert(destination.to_string(), serde_json::to_value(self).unwrap_or_default());
        extra.insert(STATE_KEY.to_string(), Value::Object(map));
    }
}
