//! One trace per colony (#846): a per-colony state machine fed the colony's `events.jsonl` lines in
//! file order (and the `activity.jsonl` outcomes that close it), emitting finished spans. Structure
//! only (P5): names, ids, times, counts, models, error classes — never a prompt, a tool's input or
//! output, a path or a command. Every value goes through the [`Policy`], like every log record.
//!
//! - **Ids** (docs/design/observability.md, Identity): the trace id is
//!   `sha256("colonizer.trace.v1|" + host_id + "|" + colony_id)[..16]` and a span id
//!   `sha256(trace_id ‖ kind ‖ key)[..8]`, keyed by the colony id (root), the turn number, the
//!   `tool_call_id` and the subagent's `agent.id`. Replaying the same lines — a restart, a re-export,
//!   a backfill — recomputes the same ids.
//! - **The tree:** `invoke_agent <repo>` (root) → `turn <n>` → `execute_tool <name>`, and a Task
//!   call's `subagent <name>` under its `execute_tool Task` span, with the subagent's own tool calls
//!   under it.
//! - **Spans leave when they close.** A turn's tool spans go out as their results arrive and the
//!   turn when it ends, so a long colony's trace fills in as it runs. The root goes out once, at the
//!   colony's final outcome (`merged`, `closed`, `no_changes`, `failed`), its deletion, or after
//!   [`ROOT_AFTER_IDLE`] of silence once it is stopped; a suspension, a restore and a stop are
//!   events on it. Whatever is still open then is closed with `colonizer.span.incomplete = true`.
//! - **Restart safety:** the open spans live in [`Traces`], which the exporter commits in the same
//!   `state.json` write as the cursors, after the backend's ack. A restart resumes mid-turn with the
//!   same ids; a crash before the commit replays the same lines over the same state, so the same
//!   spans go out again.
//! - **Sampling:** a colony is traced or not as a whole, by its trace id (`trace_sample_ratio`).
//!   Logs and metrics are never sampled.

use crate::batch::Item;
use crate::contract::ColonyPolicy;
use crate::map::ts_nanos;
use crate::policy::{Policy, Source, SpanKind, Tier};
use crate::proto::trace::v1::status::StatusCode;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

mod model;
pub(crate) use model::{ColonyTrace, Open, Ref, SpanKindName, Tokens, Traces, name_like, sampled, span_id, trace_id};
/// The most spans one colony holds open; past it the oldest is closed with `colonizer.evicted`.
pub(crate) const MAX_OPEN_SPANS: usize = 4096;
/// How long a stopped colony stays silent before its root goes out with `colonizer.outcome = idle`.
pub(crate) const ROOT_AFTER_IDLE: u64 = 24 * 3600 * 1_000_000_000;
/// The most events the root carries (suspensions, restores, stops); later ones are not recorded.
const MAX_ROOT_EVENTS: usize = 64;
/// The most ended subagents remembered, so a late line of one never reopens its span.
const MAX_ENDED_AGENTS: usize = 256;
/// The outcomes that end a colony: its root span goes out.
const FINAL_OUTCOMES: [&str; 4] = ["merged", "closed", "no_changes", "failed"];
/// The outcomes recorded as events on the root.
const ROOT_EVENTS: [&str; 3] = ["suspended", "restored", "stopped"];
/// The tool calls that start a subagent.
const TASK_TOOLS: [&str; 2] = ["Task", "Agent"];

/// What a span closes with.
#[derive(Clone, Debug, Default)]
struct Close {
    status: Option<StatusCode>,
    error_type: Option<&'static str>,
    incomplete: bool,
    unmatched: bool,
    evicted: bool,
    denial: Option<String>,
    output_bytes: Option<i64>,
    /// A turn's deltas.
    turn: Option<(Tokens, f64)>,
}

/// Builds spans from ledger lines: the policy, the install's ids and the colonies' metadata.
pub(crate) struct Builder<'a> {
    pub policy: &'a Policy,
    pub host_id: &'a str,
    pub colonies: &'a BTreeMap<String, ColonyPolicy>,
    pub sample_ratio: f64,
}

impl Builder<'_> {
    fn traced(&self, colony: &str) -> bool {
        sampled(trace_id(self.host_id, colony), self.sample_ratio)
    }

    /// One line of `source` (`events` of `colony`, or `activity`) into `state`, appending the spans
    /// it closes to `out`.
    pub(crate) fn feed(&self, state: &mut Traces, source: Source, colony: Option<&str>, line: &Value, out: &mut Vec<Item>) {
        match source {
            Source::Events => {
                if let Some(colony) = colony
                    && self.traced(colony)
                {
                    let trace = state.colonies.entry(colony.to_string()).or_default();
                    self.event(colony, trace, line, out);
                }
            }
            Source::Activity => {
                let Some(colony) = line.get("colony").and_then(Value::as_str) else {
                    return;
                };
                if !self.traced(colony) || !(self.colonies.contains_key(colony) || state.colonies.contains_key(colony)) {
                    return;
                }
                let trace = state.colonies.entry(colony.to_string()).or_default();
                activity(trace, line);
            }
            _ => {}
        }
    }

    /// The end of a tick: roots whose outcome is in and whose events are read to their end
    /// (`caught_up`), deleted colonies, and stopped colonies silent for [`ROOT_AFTER_IDLE`].
    pub(crate) fn settle(
        &self,
        state: &mut Traces,
        caught_up: &dyn Fn(&str) -> bool,
        gone: &[String],
        now: u64,
        out: &mut Vec<Item>,
    ) {
        for (colony, trace) in state.colonies.iter_mut() {
            if trace.root.emitted {
                continue;
            }
            let stopped = self.colonies.get(colony).and_then(|p| p.status.as_deref()) == Some("stopped");
            let outcome = if gone.contains(colony) {
                Some("deleted".to_string())
            } else if trace.root.outcome.is_some() && caught_up(colony) {
                trace.root.outcome.clone()
            } else if stopped && now.saturating_sub(trace.last_ts.max(trace.root.outcome_ts)) >= ROOT_AFTER_IDLE {
                Some("idle".to_string())
            } else {
                None
            };
            if let Some(outcome) = outcome {
                self.finish_root(colony, trace, &outcome, out);
            }
        }
        // A deleted colony is never read again; one done and no longer listed has nothing left.
        state
            .colonies
            .retain(|id, t| !gone.contains(id) && !(t.root.emitted && t.open.is_empty() && !self.colonies.contains_key(id)));
    }

    fn event(&self, colony: &str, trace: &mut ColonyTrace, line: &Value, out: &mut Vec<Item>) {
        let kind = line.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "assistant_text_delta" {
            return;
        }
        let ts = ts_nanos(line).unwrap_or(trace.last_ts);
        if trace.first_ts == 0 {
            trace.first_ts = ts;
        }
        trace.last_ts = trace.last_ts.max(ts);
        let agent = line
            .get("agent")
            .or_else(|| line.get("agent_ref"))
            .and_then(|a| a.get("id"))
            .and_then(Value::as_str);
        let origin = line.get("origin").and_then(Value::as_str);

        match kind {
            "model_changed" if agent.is_none() => {
                trace.model = line.get("model").and_then(Value::as_str).map(str::to_string);
            }
            "user_message" if agent.is_none() => {
                if !trace.turn_open() {
                    self.open_turn(colony, trace, ts, origin);
                }
            }
            "turn_end" if agent.is_none() => {
                if !trace.turn_open() {
                    self.open_turn(colony, trace, ts, origin);
                }
                self.end_turn(colony, trace, line, ts, out);
            }
            "assistant_text" | "thinking" | "tool_call" | "question" | "tool_result" => {
                // A background subagent's lines may come after its turn ended: they open none.
                if agent.is_none() && !trace.turn_open() {
                    self.open_turn(colony, trace, ts, None);
                }
                if let Some(id) = agent {
                    self.ensure_subagent(colony, trace, id, line, ts, out);
                }
                match kind {
                    "tool_call" => self.tool_call(colony, trace, line, agent, ts, out),
                    "tool_result" => self.tool_result(colony, trace, line, ts, out),
                    _ => {}
                }
            }
            "subagent_end" => {
                if let Some(id) = line.get("tool_call_id").and_then(Value::as_str) {
                    let failed = line.get("status").and_then(Value::as_str) == Some("failed");
                    let close = Close {
                        status: failed.then_some(StatusCode::Error),
                        error_type: failed.then_some("subagent_failed"),
                        ..Close::default()
                    };
                    self.close(colony, trace, SpanKindName::Subagent, id, ts, close, out);
                }
            }
            // The host chain and questions (#847) and content records (#848) hook in here; this
            // build traces nothing else.
            _ => {}
        }
    }

    fn open_turn(&self, colony: &str, trace: &mut ColonyTrace, ts: u64, origin: Option<&str>) {
        trace.turn += 1;
        let n = trace.turn.to_string();
        trace.open.push(Open {
            at: Ref {
                kind: SpanKindName::Turn,
                key: n.clone(),
            },
            parent: root_ref(colony),
            subject: n,
            start: ts,
            origin: origin.filter(|o| name_like(o)).map(str::to_string),
            background: false,
        });
    }

    /// The span a line belongs under: its subagent's when it has one, else the open turn's.
    fn parent(trace: &ColonyTrace, colony: &str, agent: Option<&str>) -> Ref {
        if let Some(id) = agent {
            return Ref {
                kind: SpanKindName::Subagent,
                key: id.to_string(),
            };
        }
        trace
            .open
            .iter()
            .rev()
            .find(|o| o.at.kind == SpanKindName::Turn)
            .map(|o| o.at.clone())
            .unwrap_or_else(|| root_ref(colony))
    }

    fn ensure_subagent(&self, colony: &str, trace: &mut ColonyTrace, id: &str, line: &Value, ts: u64, out: &mut Vec<Item>) {
        let name = line
            .get("agent")
            .or_else(|| line.get("agent_ref"))
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
            .filter(|n| name_like(n))
            .unwrap_or("");
        if let Some(at) = trace.find(SpanKindName::Subagent, id) {
            // Opened by its background launch, before any line named it.
            if trace.open[at].subject.is_empty() {
                trace.open[at].subject = name.to_string();
            }
            return;
        }
        if trace.ended_agents.contains(id) {
            return;
        }
        // It started with its Task call, when that call is still open.
        let start = trace
            .find(SpanKindName::ExecuteTool, id)
            .map(|i| trace.open[i].start)
            .unwrap_or(ts);
        self.push_open(
            colony,
            trace,
            Open {
                at: Ref {
                    kind: SpanKindName::Subagent,
                    key: id.to_string(),
                },
                parent: Ref {
                    kind: SpanKindName::ExecuteTool,
                    key: id.to_string(),
                },
                subject: name.to_string(),
                start,
                origin: None,
                background: false,
            },
            out,
        );
    }

    fn tool_call(&self, colony: &str, trace: &mut ColonyTrace, line: &Value, agent: Option<&str>, ts: u64, out: &mut Vec<Item>) {
        let Some(id) = line.get("tool_call_id").and_then(Value::as_str) else {
            return;
        };
        if trace.find(SpanKindName::ExecuteTool, id).is_some() {
            return;
        }
        let name = line
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| name_like(n))
            .unwrap_or("");
        let parent = Self::parent(trace, colony, agent);
        self.push_open(
            colony,
            trace,
            Open {
                at: Ref {
                    kind: SpanKindName::ExecuteTool,
                    key: id.to_string(),
                },
                parent,
                subject: name.to_string(),
                start: ts,
                origin: None,
                background: false,
            },
            out,
        );
    }

    fn tool_result(&self, colony: &str, trace: &mut ColonyTrace, line: &Value, ts: u64, out: &mut Vec<Item>) {
        let Some(id) = line.get("tool_call_id").and_then(Value::as_str) else {
            return;
        };
        let Some(at) = trace.find(SpanKindName::ExecuteTool, id) else {
            return;
        };
        let task = TASK_TOOLS.contains(&trace.open[at].subject.as_str());
        let failed = line.get("is_error").and_then(Value::as_bool) == Some(true);
        let denial = line
            .pointer("/denial/class")
            .and_then(Value::as_str)
            .filter(|c| name_like(c))
            .map(str::to_string);
        let output_bytes = line.get("output").and_then(Value::as_str).map(|o| o.len() as i64);
        let close = Close {
            status: failed.then_some(StatusCode::Error),
            error_type: failed.then_some("tool_error"),
            denial,
            output_bytes,
            ..Close::default()
        };
        let task_start = trace.open[at].start;
        let parent = trace.open[at].at.clone();
        self.close(colony, trace, SpanKindName::ExecuteTool, id, ts, close, out);
        if !task {
            return;
        }
        let background = line.get("background").and_then(Value::as_bool) == Some(true);
        match trace.find(SpanKindName::Subagent, id) {
            // Launched in the background: it ends with its `subagent_end`, not this ack.
            Some(sub) if background => trace.open[sub].background = true,
            None if background && !trace.ended_agents.contains(id) => {
                let open = Open {
                    at: Ref {
                        kind: SpanKindName::Subagent,
                        key: id.to_string(),
                    },
                    parent,
                    subject: String::new(),
                    start: task_start,
                    origin: None,
                    background: true,
                };
                self.push_open(colony, trace, open, out);
            }
            None => {}
            Some(_) => {
                let close = Close {
                    status: failed.then_some(StatusCode::Error),
                    error_type: failed.then_some("subagent_failed"),
                    ..Close::default()
                };
                self.close(colony, trace, SpanKindName::Subagent, id, ts, close, out);
            }
        }
    }

    fn end_turn(&self, colony: &str, trace: &mut ColonyTrace, line: &Value, ts: u64, out: &mut Vec<Item>) {
        // What the turn left open closes with it, unmatched — but a background subagent, and the
        // tool calls under it, run on.
        let background: BTreeSet<String> = trace
            .open
            .iter()
            .filter(|o| o.background && o.at.kind == SpanKindName::Subagent)
            .map(|o| o.at.key.clone())
            .collect();
        let stale: Vec<Ref> = trace
            .open
            .iter()
            .rev()
            .filter(|o| o.at.kind != SpanKindName::Turn)
            .filter(|o| {
                !background.contains(&o.at.key)
                    && !(o.parent.kind == SpanKindName::Subagent && background.contains(&o.parent.key))
            })
            .map(|o| o.at.clone())
            .collect();
        for at in stale {
            let close = Close {
                unmatched: true,
                ..Close::default()
            };
            self.close(colony, trace, at.kind, &at.key, ts, close, out);
        }
        let tokens = Tokens::of(line.get("model_usage")).unwrap_or(trace.tokens);
        let cost = line.get("cost_usd").and_then(Value::as_f64).unwrap_or(trace.cost_usd);
        // Rounded to a billionth of a dollar, so 0.8 − 0.5 reads 0.3 and the turns still sum up.
        let cost_delta = ((cost - trace.cost_usd).max(0.0) * 1e9).round() / 1e9;
        let delta = (tokens.saturating_sub(trace.tokens), cost_delta);
        trace.tokens = tokens;
        trace.cost_usd = cost;
        let failed = line.get("is_error").and_then(Value::as_bool) == Some(true);
        let close = Close {
            status: failed.then_some(StatusCode::Error),
            error_type: failed.then_some("turn_error"),
            turn: Some(delta),
            ..Close::default()
        };
        let n = trace.turn.to_string();
        self.close(colony, trace, SpanKindName::Turn, &n, ts, close, out);
    }

    /// Opens a span, closing the oldest open one (never a turn) past [`MAX_OPEN_SPANS`].
    fn push_open(&self, colony: &str, trace: &mut ColonyTrace, open: Open, out: &mut Vec<Item>) {
        trace.open.push(open);
        while trace.open.len() > MAX_OPEN_SPANS {
            let Some(oldest) = trace
                .open
                .iter()
                .find(|o| o.at.kind != SpanKindName::Turn)
                .map(|o| o.at.clone())
            else {
                break;
            };
            let at = trace.last_ts;
            let close = Close {
                evicted: true,
                ..Close::default()
            };
            self.close(colony, trace, oldest.kind, &oldest.key, at, close, out);
        }
    }

    /// Ends the open span (kind, key) at `end` and emits it.
    #[allow(clippy::too_many_arguments)]
    fn close(
        &self,
        colony: &str,
        trace: &mut ColonyTrace,
        kind: SpanKindName,
        key: &str,
        end: u64,
        close: Close,
        out: &mut Vec<Item>,
    ) {
        let Some(at) = trace.find(kind, key) else { return };
        let open = trace.open.remove(at);
        if kind == SpanKindName::Subagent {
            if trace.ended_agents.len() >= MAX_ENDED_AGENTS
                && let Some(first) = trace.ended_agents.iter().next().cloned()
            {
                trace.ended_agents.remove(&first);
            }
            trace.ended_agents.insert(key.to_string());
        }
        out.push(self.span(colony, trace, &open, end.max(open.start), &close));
    }

    fn span(&self, colony: &str, trace: &ColonyTrace, open: &Open, end: u64, close: &Close) -> Item {
        let tid = trace_id(self.host_id, colony);
        let kind = open.at.kind.kind();
        let mut span = self
            .policy
            .span(kind, &open.subject)
            .ids(
                tid,
                span_id(tid, kind, &open.at.key),
                Some(span_id(tid, open.parent.kind.kind(), &open.parent.key)),
            )
            .times(open.start, end);
        let s = Tier::Structure;
        match open.at.kind {
            SpanKindName::Turn => {
                if let Some(origin) = &open.origin {
                    span = span.attr("colonizer.origin", origin.as_str(), s);
                }
                if let Some(model) = trace.model.as_deref().filter(|m| name_like(m)) {
                    span = span.attr("gen_ai.response.model", model, s);
                }
                if let Some((tokens, cost)) = close.turn {
                    span = span
                        .attr("gen_ai.usage.input_tokens", tokens.input as i64, s)
                        .attr("gen_ai.usage.output_tokens", tokens.output as i64, s)
                        .attr("gen_ai.usage.cache_read.input_tokens", tokens.cache_read as i64, s)
                        .attr("gen_ai.usage.cache_creation.input_tokens", tokens.cache_write as i64, s)
                        .attr("colonizer.cost_usd", cost, s);
                }
            }
            SpanKindName::ExecuteTool => {
                span = span
                    .attr("gen_ai.operation.name", "execute_tool", s)
                    .attr("gen_ai.tool.call.id", open.at.key.as_str(), s);
                if !open.subject.is_empty() {
                    span = span.attr("gen_ai.tool.name", open.subject.as_str(), s);
                }
                if let Some(class) = &close.denial {
                    span = span.attr("colonizer.denial.class", class.as_str(), s);
                }
                if let Some(bytes) = close.output_bytes {
                    span = span.attr("colonizer.tool.output_bytes", bytes, s);
                }
            }
            SpanKindName::Subagent => {
                span = span
                    .attr("gen_ai.operation.name", "invoke_agent", s)
                    .attr("gen_ai.agent.id", open.at.key.as_str(), s);
                if !open.subject.is_empty() {
                    span = span.attr("gen_ai.agent.name", open.subject.as_str(), s);
                }
            }
            _ => {}
        }
        if let Some(error) = close.error_type {
            span = span.attr("error.type", error, s);
        }
        for (flag, key) in [
            (close.incomplete, "colonizer.span.incomplete"),
            (close.unmatched, "colonizer.unmatched"),
            (close.evicted, "colonizer.evicted"),
        ] {
            if flag {
                span = span.attr(key, true, s);
            }
        }
        let status = match close.status {
            Some(code) => code,
            None if close.incomplete || close.unmatched || close.evicted => StatusCode::Unset,
            None if open.at.kind == SpanKindName::Turn || open.at.kind == SpanKindName::ExecuteTool => StatusCode::Ok,
            None => StatusCode::Unset,
        };
        span.status(status).finish()
    }

    /// Closes everything still open as incomplete, then emits the root, once.
    fn finish_root(&self, colony: &str, trace: &mut ColonyTrace, outcome: &str, out: &mut Vec<Item>) {
        let end = trace.last_ts.max(trace.root.outcome_ts);
        let at = trace.last_ts;
        while let Some(open) = trace.open.last().map(|o| o.at.clone()) {
            let close = Close {
                incomplete: true,
                ..Close::default()
            };
            self.close(colony, trace, open.kind, &open.key, at, close, out);
        }
        let policy = self.colonies.get(colony);
        let start = policy
            .and_then(|p| p.created_at.as_deref())
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .and_then(|t| t.timestamp_nanos_opt())
            .and_then(|t| u64::try_from(t).ok())
            .unwrap_or(trace.first_ts);
        let tid = trace_id(self.host_id, colony);
        let repo = policy.map(|p| p.repo.as_str()).unwrap_or("");
        let s = Tier::Structure;
        let mut span = self
            .policy
            .span(SpanKind::InvokeAgent, repo)
            .ids(tid, span_id(tid, SpanKind::InvokeAgent, colony), None)
            .times(start, end.max(start))
            .attr("gen_ai.operation.name", "invoke_agent", s)
            .attr("gen_ai.agent.id", colony, s)
            .attr("colonizer.colony.id", colony, s)
            .attr("colonizer.outcome", outcome, s)
            .attr("gen_ai.usage.input_tokens", trace.tokens.input as i64, s)
            .attr("gen_ai.usage.output_tokens", trace.tokens.output as i64, s)
            .attr("gen_ai.usage.cache_read.input_tokens", trace.tokens.cache_read as i64, s)
            .attr("gen_ai.usage.cache_creation.input_tokens", trace.tokens.cache_write as i64, s)
            .attr("colonizer.cost_usd", trace.cost_usd, s);
        if let Some(p) = policy {
            if name_like(&p.agent) {
                span = span.attr("gen_ai.agent.name", p.agent.as_str(), s);
            }
            if !p.repo.is_empty() {
                span = span.attr("colonizer.repo", p.repo.as_str(), s);
            }
            if !p.org.is_empty() {
                span = span.attr("colonizer.org", p.org.as_str(), s);
            }
            span = span.attr(
                "colonizer.origin",
                p.origin.as_deref().filter(|o| name_like(o)).unwrap_or("user"),
                s,
            );
            if let Some(url) = p.pr_url.as_deref().filter(|u| !u.is_empty()) {
                span = span.attr("colonizer.pr.url", url, s);
            }
        }
        if let Some(model) = trace.model.as_deref().filter(|m| name_like(m)) {
            span = span.attr("gen_ai.response.model", model, s);
        }
        for (name, at) in &trace.root.events {
            span = span.event(name, *at);
        }
        let status = match outcome {
            "failed" => {
                span = span.attr("error.type", "failed", s);
                StatusCode::Error
            }
            "merged" | "closed" | "no_changes" => StatusCode::Ok,
            _ => StatusCode::Unset,
        };
        out.push(span.status(status).finish());
        trace.root.emitted = true;
        trace.root.events.clear();
    }
}

fn root_ref(colony: &str) -> Ref {
    Ref {
        kind: SpanKindName::InvokeAgent,
        key: colony.to_string(),
    }
}

/// An `activity.jsonl` line about a traced colony: a final outcome, or an event for its root.
fn activity(trace: &mut ColonyTrace, line: &Value) {
    let Some(what) = line
        .get("kind")
        .and_then(Value::as_str)
        .and_then(|k| k.strip_prefix("outcome."))
    else {
        return;
    };
    let ts = ts_nanos(line).unwrap_or(trace.last_ts);
    if FINAL_OUTCOMES.contains(&what) {
        if !trace.root.emitted && trace.root.outcome.is_none() {
            trace.root.outcome = Some(what.to_string());
            trace.root.outcome_ts = ts;
        }
    } else if ROOT_EVENTS.contains(&what) {
        trace.root.outcome_ts = trace.root.outcome_ts.max(ts);
        if !trace.root.emitted && trace.root.events.len() < MAX_ROOT_EVENTS {
            trace.root.events.push((format!("colonizer.{what}"), ts));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
