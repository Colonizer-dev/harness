//! The attribute keys each record kind may carry (P2): the source inventory's and the span tree's
//! allowlists in docs/design/observability.md, plus the keys every record shares. A key ending in
//! `.*` matches any key under that prefix (`model_usage.*` → `model_usage.input_tokens`).
//!
//! Each key also says its tier — a key the design names as content is content whatever tier the
//! caller passes — and how `repo_names = hashed` treats it: hashed (a repository, an org, a path),
//! dropped (a branch, a URL naming the repository, a title), or left alone.

use super::Tier;

/// A record's source: the first column of the design's source inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Events,
    Harness,
    Gateway,
    Findings,
    Activity,
    Spend,
    Decisions,
    Routing,
    JevLadder,
    JevFocus,
    Mothership,
}

impl Source {
    pub const ALL: [Source; 11] = [
        Source::Events,
        Source::Harness,
        Source::Gateway,
        Source::Findings,
        Source::Activity,
        Source::Spend,
        Source::Decisions,
        Source::Routing,
        Source::JevLadder,
        Source::JevFocus,
        Source::Mothership,
    ];

    /// The identifier record ids are computed over (`colonizer.rec.v1|…|<source>|…`).
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Events => "events",
            Source::Harness => "harness",
            Source::Gateway => "gateway",
            Source::Findings => "findings",
            Source::Activity => "activity",
            Source::Spend => "spend",
            Source::Decisions => "decisions",
            Source::Routing => "routing",
            Source::JevLadder => "jev_ladder",
            Source::JevFocus => "jev_focus",
            Source::Mothership => "mothership",
        }
    }
}

/// A span's kind: the design's closed, prefix-free set that span ids are computed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpanKind {
    InvokeAgent,
    Turn,
    Subagent,
    ExecuteTool,
    Chat,
    Question,
    HostStep,
}

impl SpanKind {
    pub const ALL: [SpanKind; 7] = [
        SpanKind::InvokeAgent,
        SpanKind::Turn,
        SpanKind::Subagent,
        SpanKind::ExecuteTool,
        SpanKind::Chat,
        SpanKind::Question,
        SpanKind::HostStep,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SpanKind::InvokeAgent => "invoke_agent",
            SpanKind::Turn => "turn",
            SpanKind::Subagent => "subagent",
            SpanKind::ExecuteTool => "execute_tool",
            SpanKind::Chat => "chat",
            SpanKind::Question => "question",
            SpanKind::HostStep => "host_step",
        }
    }
}

/// What `repo_names = hashed` does to a key's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Naming {
    /// Not a name: unchanged.
    Plain,
    /// A repository, org or path: replaced by its keyed hash.
    Hashed,
    /// A branch, a URL naming the repository, a title: dropped outright.
    Dropped,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Rule {
    pub key: &'static str,
    pub tier: Tier,
    pub naming: Naming,
}

const fn s(key: &'static str) -> Rule {
    Rule {
        key,
        tier: Tier::Structure,
        naming: Naming::Plain,
    }
}
const fn h(key: &'static str) -> Rule {
    Rule {
        key,
        tier: Tier::Structure,
        naming: Naming::Hashed,
    }
}
const fn d(key: &'static str) -> Rule {
    Rule {
        key,
        tier: Tier::Structure,
        naming: Naming::Dropped,
    }
}
const fn c(key: &'static str) -> Rule {
    Rule {
        key,
        tier: Tier::Content,
        naming: Naming::Plain,
    }
}
const fn ch(key: &'static str) -> Rule {
    Rule {
        key,
        tier: Tier::Content,
        naming: Naming::Hashed,
    }
}
const fn cd(key: &'static str) -> Rule {
    Rule {
        key,
        tier: Tier::Content,
        naming: Naming::Dropped,
    }
}

/// Set by the policy itself on a record it truncated.
pub const TRUNCATED: &str = "colonizer.truncated";

/// Every log record and metric point may carry these: ids, the colony attribute join, and the
/// policy's own marker.
const RECORD_COMMON: &[Rule] = &[
    s("colonizer.record.id"),
    s("colonizer.colony.id"),
    s("colonizer.source"),
    s("colonizer.stream"),
    s("colonizer.trace.id"),
    s("colonizer.span.id"),
    h("colonizer.org"),
    h("colonizer.repo"),
    d("colonizer.branch"),
    d("colonizer.pr.url"),
    cd("colonizer.issue.title"),
    cd("colonizer.pr.title"),
    s(TRUNCATED),
];

/// Every span may carry these. The `gen_ai.*` keys are the structural ones only — names, ids,
/// models, token counts — never the semantic conventions' message or argument keys (P5).
const SPAN_COMMON: &[Rule] = &[
    s(TRUNCATED),
    s("colonizer.span.incomplete"),
    s("gen_ai.operation.name"),
    s("gen_ai.provider.name"),
    s("gen_ai.system"),
    s("gen_ai.request.model"),
    s("gen_ai.response.model"),
    s("gen_ai.usage.input_tokens"),
    s("gen_ai.usage.output_tokens"),
    s("gen_ai.agent.id"),
    s("gen_ai.agent.name"),
    s("gen_ai.tool.name"),
    s("gen_ai.tool.call.id"),
    s("gen_ai.conversation.id"),
];

const EVENTS: &[Rule] = &[
    s("type"),
    s("state"),
    s("risk"),
    s("kind"),
    s("blocking"),
    s("tool_call_id"),
    s("name"),
    s("is_error"),
    s("denial.class"),
    s("question_id"),
    s("model"),
    s("model_usage.*"),
    s("cost_usd"),
    s("duration_ms"),
    s("access"),
    s("policy"),
    // A file path is content (P3) and a repository name (P9): gated, and hashed when `hashed`.
    ch("path"),
    s("agent_ref.id"),
    s("agent_ref.name"),
];

const HARNESS: &[Rule] = &[s("origin"), s("level")];

const GATEWAY: &[Rule] = &[
    s("provider"),
    s("wire"),
    s("model"),
    s("wire_model"),
    s("status"),
    s("failure"),
    s("fallback"),
    s("queue_ms"),
    s("duration_ms"),
    s("request_bytes"),
    s("response_bytes"),
    s("input_tokens"),
    s("output_tokens"),
];

/// `issue`, `duplicate_of` and `pr` are URLs that name the repository, so `hashed` drops them.
const FINDINGS: &[Rule] = &[
    s("state"),
    d("issue"),
    d("duplicate_of"),
    s("severity"),
    s("verdict"),
    s("fix_session"),
    s("review_session"),
    d("pr"),
    cd("title"),
    c("reason"),
];

// `target` names the org for every `workspace.*` entry (crates/colonizer/src/activity.rs), so it is
// hashed like one.
const ACTIVITY: &[Rule] = &[s("kind"), s("actor"), s("colony"), h("repo"), h("target"), c("detail")];

const SPEND: &[Rule] = &[
    s("kind"),
    h("org"),
    s("session"),
    s("agent"),
    s("model"),
    s("input_tokens"),
    s("output_tokens"),
    s("cache_read_tokens"),
    s("cache_write_tokens"),
    s("cost_usd"),
    s("scoring_ms"),
];

/// `decisions` and `routing` share a row shape. `options` is a count, never the options.
const DECISIONS: &[Rule] = &[
    s("point"),
    s("mode"),
    s("options"),
    s("pick"),
    s("confidence"),
    s("latency_ms"),
    s("miss"),
    s("did"),
    s("outcome"),
];

const JEV_LADDER: &[Rule] = &[
    s("kind"),
    s("tool"),
    s("tool_call_id"),
    s("action"),
    s("keep_call"),
    s("keep_result"),
    s("matched_tool_call_id"),
];

/// `verify_focus.rs`'s row, free text excluded; `candidates` is a count.
const JEV_FOCUS: &[Rule] = &[
    s("kind"),
    s("session"),
    s("mode"),
    s("candidates"),
    s("chosen"),
    s("would_catch"),
    s("verdict"),
    s("actual_first_failure_ms"),
    s("focused_first_failure_ms"),
    s("total_ms"),
    s("checks_run"),
];

/// `fields.*` stays off until #856 names the field keys it allows.
const MOTHERSHIP: &[Rule] = &[s("level"), s("target")];

const INVOKE_AGENT: &[Rule] = &[
    s("colonizer.colony.id"),
    h("colonizer.repo"),
    s("colonizer.outcome"),
    s("colonizer.trace.dropped_spans"),
];
const TURN: &[Rule] = &[s("cost_usd"), s("is_error")];
/// The description is content, and content is never a span attribute (P5): always dropped here.
const SUBAGENT: &[Rule] = &[c("colonizer.subagent.description")];
const EXECUTE_TOOL: &[Rule] = &[
    s("tool.name"),
    s("is_error"),
    s("denial.class"),
    s("colonizer.tool.output_bytes"),
];
const CHAT: &[Rule] = &[
    s("input_tokens"),
    s("output_tokens"),
    s("status"),
    s("failure"),
    s("fallback"),
];
const QUESTION: &[Rule] = &[s("risk"), s("kind"), s("blocking")];
const HOST_STEP: &[Rule] = &[s("kind"), s("actor")];

/// The tables a source's log records and metric points are checked against.
pub(crate) fn for_source(source: Source) -> [&'static [Rule]; 2] {
    let own = match source {
        Source::Events => EVENTS,
        Source::Harness => HARNESS,
        Source::Gateway => GATEWAY,
        Source::Findings => FINDINGS,
        Source::Activity => ACTIVITY,
        Source::Spend => SPEND,
        Source::Decisions | Source::Routing => DECISIONS,
        Source::JevLadder => JEV_LADDER,
        Source::JevFocus => JEV_FOCUS,
        Source::Mothership => MOTHERSHIP,
    };
    [RECORD_COMMON, own]
}

/// The tables a span of `kind` is checked against.
pub(crate) fn for_span(kind: SpanKind) -> [&'static [Rule]; 2] {
    let own = match kind {
        SpanKind::InvokeAgent => INVOKE_AGENT,
        SpanKind::Turn => TURN,
        SpanKind::Subagent => SUBAGENT,
        SpanKind::ExecuteTool => EXECUTE_TOOL,
        SpanKind::Chat => CHAT,
        SpanKind::Question => QUESTION,
        SpanKind::HostStep => HOST_STEP,
    };
    [SPAN_COMMON, own]
}

/// `key`'s rule in `tables`, the first match winning.
pub(crate) fn lookup(tables: &[&'static [Rule]], key: &str) -> Option<Rule> {
    tables.iter().flat_map(|t| t.iter()).copied().find(|r| matches(r.key, key))
}

fn matches(pattern: &str, key: &str) -> bool {
    match pattern.strip_suffix('*') {
        // The rest of a prefixed key is a name, never free text: short, and only name characters.
        Some(prefix) => key.strip_prefix(prefix).is_some_and(|rest| {
            !rest.is_empty()
                && rest.len() <= 64
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        }),
        None => pattern == key,
    }
}

/// A concrete key for every entry of `tables` (a prefix pattern gets one example), for tests that
/// place a value at every allowed position.
#[cfg(test)]
pub(crate) fn every_key(tables: &[&'static [Rule]]) -> Vec<(String, Tier, Naming)> {
    tables
        .iter()
        .flat_map(|t| t.iter())
        .map(|r| {
            let key = match r.key.strip_suffix('*') {
                Some(prefix) => format!("{prefix}example"),
                None => r.key.to_string(),
            };
            (key, r.tier, r.naming)
        })
        .collect()
}
