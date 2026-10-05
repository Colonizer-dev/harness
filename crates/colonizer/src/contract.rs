//! The surface the repository-level checks in `crates/repo-contracts` compile against.
//!
//! This is not a stable API. It exists so a check that has to read the whole repository — docs
//! against code, schemas against types, module manifests against presets, a shared fixture summed
//! on both sides of the language split — can live in its own (unpublished) crate, where opening
//! files a published crate's own tests must not depend on is the whole point, while still
//! asserting against this crate's code. Items are raised to `pub` here rather than in their own
//! modules only when a check needs them; nothing else in the crate calls through this module, so
//! it can change the moment a check does.

pub use crate::events::resolve_origin;
pub use crate::fleet_export::{BUNDLE_FORMAT, BUNDLE_VERSION};
pub use crate::modules::{DeclaredSecret, Requires, check_requires, discover_agents, parse_requires, read_agent};
pub use crate::plugins::validate;
pub use crate::presets::{detect, find, pinned_image};
pub use crate::protocol::Origin;
pub use crate::util::short_id;

/// Whether a module's runner applies the exec policy (`sessions::agentd`): its settings schema
/// declares an `exec_policy` property. Wrapped rather than re-exported because `agentd`'s item is
/// reachable at `pub(crate)` only.
pub fn applies_exec_policy(schema: &serde_json::Value) -> bool {
    crate::sessions::applies_exec_policy(schema)
}

/// Whether the mothership accepts an exec policy's JSON text at a save (`exec_policy::validate`,
/// issue #924), for the shared-fixture check against the runner's `parsePolicy`. Wrapped because
/// the item is `pub(crate)`.
pub fn validate_exec_policy(text: &str) -> Result<(), String> {
    crate::exec_policy::validate(text)
}

/// The boundary kinds the harness reads (`boundary::KINDS`), for the check against the schema's
/// `boundary.kind` enum. Copied rather than re-exported because the item is `pub(crate)`.
pub const BOUNDARY_KINDS: [&str; 7] = crate::boundary::KINDS;

/// The event a boundary the mothership observed writes (`Boundary::new(..).to_event()`), for the
/// check that it carries every field the schema requires. Wrapped because `new` is `pub(crate)`.
pub fn boundary_event(kind: &str, control: &str, detail: &str, target: Option<&str>) -> serde_json::Value {
    crate::boundary::Boundary::new(kind, control, detail, target).to_event()
}

/// One line of the runner contract fixture reduced to the primitives the repository-level check
/// reads. [`crate::protocol::AgentEvent`] stays crate-private — raising it would drag its whole
/// field-type graph (`AgentState`, `QuestionRisk`, `JevDecision`, …) public — so this mirrors the
/// acted-on subset the check asserts on instead.
#[derive(Clone, Debug, PartialEq)]
pub enum EventShape {
    Status {
        state: &'static str,
        detail: Option<String>,
    },
    UserMessage {
        id: String,
        text: String,
    },
    Question {
        question_id: String,
        question_count: usize,
        message_id: Option<String>,
    },
    QuestionAnswered {
        question_id: String,
        response: Option<String>,
    },
    AgentSession {
        session_id: String,
    },
    TurnEnd {
        is_error: bool,
        cost_usd: Option<f64>,
        model_usage: Option<serde_json::Value>,
    },
    MemoryProposal {
        scope: Option<String>,
        tags: Vec<String>,
    },
    Finding {
        title: String,
        evidence: String,
    },
    PathPolicy {
        access: String,
        policy: String,
        path: String,
        tool: String,
    },
    Boundary {
        kind: String,
        control: String,
        target: Option<String>,
    },
    Other,
}

/// The variant a fixture line lands on, with the fields the check reads. Panics on a line the
/// schema does not satisfy, exactly as the check did in-crate.
pub fn event_shape(line: &str) -> EventShape {
    use crate::protocol::AgentEvent;
    match serde_json::from_str::<AgentEvent>(line).expect("fixture line satisfies docs/agent-events.schema.json") {
        AgentEvent::Status { state, detail } => EventShape::Status {
            state: state.as_str(),
            detail,
        },
        AgentEvent::UserMessage { id, text } => EventShape::UserMessage { id, text },
        AgentEvent::Question {
            question_id,
            questions,
            message_id,
            ..
        } => EventShape::Question {
            question_id,
            question_count: questions.len(),
            message_id,
        },
        AgentEvent::QuestionAnswered {
            question_id, response, ..
        } => EventShape::QuestionAnswered { question_id, response },
        AgentEvent::AgentSession { session_id } => EventShape::AgentSession { session_id },
        AgentEvent::TurnEnd {
            is_error,
            cost_usd,
            model_usage,
            ..
        } => EventShape::TurnEnd {
            is_error,
            cost_usd,
            model_usage,
        },
        AgentEvent::MemoryProposal { scope, tags, .. } => EventShape::MemoryProposal { scope, tags },
        AgentEvent::Finding { title, evidence, .. } => EventShape::Finding { title, evidence },
        AgentEvent::PathPolicy {
            access,
            policy,
            path,
            tool,
        } => EventShape::PathPolicy {
            access,
            policy,
            path,
            tool,
        },
        AgentEvent::Boundary {
            kind, control, target, ..
        } => EventShape::Boundary { kind, control, target },
        _ => EventShape::Other,
    }
}

/// The numbers the shared spend fixture sums to, in primitives — `spend.rs` keeps its row types
/// private, so the repository-level check reads them through here instead.
#[derive(Debug)]
pub struct SpendFixtureTotals {
    pub days: usize,
    pub cost_usd: f64,
    pub routed_cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

/// One spend-journal row reduced to the fields the repository-level check reads.
#[derive(Debug)]
pub struct SpendFixtureRow {
    pub day: String,
    pub org: String,
    pub session: Option<String>,
    pub agent: Option<String>,
}

/// The `spend.jsonl` path under `data_dir`, where the check writes the shared fixture.
pub fn spend_journal_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    crate::spend::spend_file(data_dir)
}

/// The fixture's totals over the fixed two-day window the check pins (2026-09-14/15, read from
/// 2026-09-30), summed the way `GET /api/spend/history` sums them.
pub fn spend_fixture_totals(data_dir: &std::path::Path) -> SpendFixtureTotals {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 9, 30).expect("a real date");
    let days = crate::spend::journal_days(data_dir, today, 30, 0);
    let (mut cost, mut routed, mut input, mut output, mut cache_read, mut cache_write) = (0.0, 0.0, 0u64, 0u64, 0u64, 0u64);
    for (_, orgs) in &days {
        for (_, org) in orgs {
            cost += org.spend.cost_usd.unwrap_or_default();
            routed += org.spend.routed_cost_usd.unwrap_or_default();
            input += org.spend.input_tokens;
            output += org.spend.output_tokens;
            cache_read += org.spend.cache_read_tokens;
            cache_write += org.spend.cache_write_tokens;
        }
    }
    SpendFixtureTotals {
        days: days.len(),
        cost_usd: cost,
        routed_cost_usd: routed,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    }
}

/// The fixture's journal rows, read back through the same window the endpoint uses.
pub fn spend_fixture_rows(data_dir: &std::path::Path) -> Vec<SpendFixtureRow> {
    crate::spend::read_journal(data_dir, "0000-01-01", "2026-09-30", 0)
        .into_iter()
        .map(|row| SpendFixtureRow {
            day: row.day,
            org: row.org,
            session: row.session,
            agent: row.agent,
        })
        .collect()
}
