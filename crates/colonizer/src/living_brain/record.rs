//! A colony as a Living Brain `agent_log` record — the one place the mapping lives.
//!
//! The record is the brain's normalised session, schema version 3 (Livingbrain-wiki/livingbrain,
//! `docs/logs.md` and `crates/livingbrain-cli/src/logs.rs`), with the `colonizer` collector's
//! mapping: one record per colony, read from its `sessions.json` row (serialised exactly as the
//! store writes it) and its event logs (`events-1.jsonl` … then `events.jsonl`). Field order is wire
//! order and nothing reads the clock or the environment, so one colony in one state is one byte
//! string — what the brain's `(scope, sha256(body))` idempotency rests on.
//!
//! What a record keeps of the stream: the maintainer's instructions as the opening user turn (the
//! harness prompt around them is boilerplate), the person's own messages and answers, the
//! orchestrating agent's text, thinking and tool calls with their results, and the colony's
//! milestones as `event` turns. Harness nudges become one-line events. A subagent's own steps — its
//! text, thinking and tool calls, and the subagents it starts in turn — nest under the call that
//! started it as that call's `subagent` (schema version 3): its name, the model its messages ran
//! on, when it ran, how it ended, and its turns. Its last words are still that call's output.
//!
//! Then, in the CLI's order: the detail level (what it leaves out is never touched again), home
//! elision, the per-message clip, redaction of every string value on its own, the size cap, and one
//! last redaction pass over the serialised body. Redaction here is the mothership's: every saved
//! secret's value becomes `[REDACTED:secret]` and `colonizer-redact`'s detectors run; the brain runs
//! its own again on arrival.

use crate::sessions::Session;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// 3: a subagent's steps nest under the call that started it, and assistant turns name their model
/// when it changes.
pub(crate) const SCHEMA_VERSION: u32 = 3;
pub(crate) const AGENT: &str = "colonizer";
/// 2: subagents mapped (schema 3).
pub(crate) const COLLECTOR_VERSION: u32 = 2;
/// The most text one turn keeps under `summary`, in characters.
const SUMMARY_TURN_CHARS: usize = 2000;
/// The most of one message any level keeps: its first and last 128 KiB.
const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_TOOL_CALLS: usize = 128;
/// A tool result keeps its first and last 2 KiB, an input its first and last 8 KiB.
const MAX_TOOL_OUTPUT_BYTES: usize = 4 * 1024;
const MAX_TOOL_INPUT_BYTES: usize = 16 * 1024;
/// Three quarters of the brain's 1 MiB redaction input cap, and what fits the 1 MiB response edge
/// once escaped into a JSON string.
pub(crate) const MAX_BODY_BYTES: usize = 1024 * 1024 * 3 / 4;
const MAX_ESCAPED_BODY_BYTES: usize = 1024 * 1024 - 32 * 1024;

/// How much of a colony travels — the CLI's levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Detail {
    /// Messages cut to 2 000 characters, tool names only.
    Summary,
    /// Every message in full, tool names only — no tool input or output, no reasoning.
    Messages,
    /// Every message, reasoning summary and tool call with input and result.
    #[default]
    Full,
}

impl Detail {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Messages => "messages",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionRecord {
    schema_version: u32,
    pub(crate) agent: &'static str,
    collector_version: u32,
    pub(crate) detail: Detail,
    pub(crate) session_id: String,
    pub(crate) repo: String,
    pub(crate) branch: Option<String>,
    pub(crate) models: Vec<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) ended_at: Option<String>,
    pub(crate) turns: Vec<Turn>,
    pub(crate) totals: Totals,
    pub(crate) outcome: Option<String>,
    pub(crate) truncated: bool,
    pub(crate) omitted_turns: usize,
    /// How many subagent turns the size cap removed, across every subagent; each subagent says its
    /// own share in its `omitted_turns`.
    pub(crate) omitted_subagent_turns: usize,
    pub(crate) colony: Colony,
}

/// The colony's own metadata. Field order is wire order; absent or empty is `null`.
#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Colony {
    pub(crate) org: String,
    pub(crate) base: Option<String>,
    pub(crate) issue: Option<u64>,
    pub(crate) issue_title: Option<String>,
    pub(crate) pr_url: Option<String>,
    pub(crate) pr_opened_at: Option<String>,
    pub(crate) merged_at: Option<String>,
    pub(crate) ci_state: Option<String>,
    pub(crate) origin: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Role {
    User,
    Assistant,
    Reasoning,
    Event,
}

#[derive(Debug, Serialize)]
pub(crate) struct Turn {
    pub(crate) role: Role,
    pub(crate) at: Option<String>,
    /// On an assistant turn, the model it ran on — only where that differs from the previous
    /// assistant turn's in the same list, so the first carries it and each change after.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    pub(crate) text: String,
    pub(crate) tool_calls: Vec<ToolCall>,
}

impl Turn {
    fn new(role: Role, at: &str, text: &str) -> Self {
        Turn {
            role,
            at: present(at),
            model: None,
            text: text.to_owned(),
            tool_calls: Vec::new(),
        }
    }
}

/// A subagent a call started, and everything it did. Field order is wire order; every field is
/// always present.
#[derive(Debug, Serialize)]
pub(crate) struct Subagent {
    /// The call that started it.
    pub(crate) id: String,
    /// The subagent type the call named (`Explore`, `general-purpose`, …), else its description.
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// The models its messages ran on, first seen first; else the model the call asked for.
    pub(crate) models: Vec<String>,
    pub(crate) started_at: Option<String>,
    pub(crate) ended_at: Option<String>,
    /// `completed`, `failed`, `stopped`, or `null` when the stream never said.
    pub(crate) status: Option<&'static str>,
    /// The agent events carry no per-subagent usage: zeros and a null cost, the colony's own
    /// totals holding every model's spend.
    pub(crate) totals: Totals,
    pub(crate) turns: Vec<Turn>,
    pub(crate) truncated: bool,
    pub(crate) omitted_turns: usize,
}

/// A tool the agent called; under `full`, what it was called with and what came back.
#[derive(Debug, Default, Serialize)]
pub(crate) struct ToolCall {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<i64>,
    /// The subagent this call started, when it started one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subagent: Option<Box<Subagent>>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Totals {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_creation_tokens: u64,
    pub(crate) reasoning_tokens: u64,
    pub(crate) cost_usd: Option<f64>,
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn num_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn present(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn opt(value: &Value, key: &str) -> Option<String> {
    present(str_field(value, key))
}

fn truncate_text(text: &mut String, max: usize) {
    if let Some((cut, _)) = text.char_indices().nth(max) {
        text.truncate(cut);
    }
}

/// `text` within `max` bytes: its head and tail around a `… N bytes omitted …` line.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let half = max / 2;
    let mut head_end = half;
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - half;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let omitted = tail_start - head_end;
    format!("{}\n… {omitted} bytes omitted …\n{}", &text[..head_end], &text[tail_start..])
}

fn note_model(record: &mut SessionRecord, model: &str) {
    if !model.is_empty() && !record.models.iter().any(|seen| seen == model) {
        record.models.push(model.to_owned());
    }
}

fn note_time(record: &mut SessionRecord, at: &str) {
    if at.is_empty() {
        return;
    }
    if record.started_at.is_none() {
        record.started_at = Some(at.to_owned());
    }
    record.ended_at = Some(at.to_owned());
}

/// An event turn — unless the turn before it is the same event.
fn push_event(record: &mut SessionRecord, at: &str, text: &str) {
    if let Some(last) = record.turns.last()
        && last.role == Role::Event
        && last.text == text
    {
        return;
    }
    record.turns.push(Turn::new(Role::Event, at, text));
}

// ---------------------------------------------------------------------------------------------
// Tool calls (the CLI's `tools.rs`)

const SHELL: &str = "shell";
const DIFF: &str = "diff";
const JSON: &str = "json";
const TEXT: &str = "text";

fn describe(name: &str, input: &Value) -> ToolCall {
    let (format, path, text) = shape(name, input);
    ToolCall {
        name: name.to_owned(),
        format: text.as_ref().map(|_| format),
        path: path.filter(|path| !path.is_empty()),
        input: text
            .filter(|text| !text.is_empty())
            .map(|text| clip(&text, MAX_TOOL_INPUT_BYTES)),
        ..ToolCall::default()
    }
}

fn shape(name: &str, input: &Value) -> (&'static str, Option<String>, Option<String>) {
    let s = |key: &str| input.get(key).and_then(Value::as_str).map(str::to_owned);
    let path = s("file_path").or_else(|| s("notebook_path")).or_else(|| s("path"));
    match name {
        "Bash" | "exec_command" | "shell" | "container.exec" | "local_shell" | "shell_command" | "unified_exec" => {
            let command = input
                .get("command")
                .or_else(|| input.get("cmd"))
                .map(command_text)
                .unwrap_or_default();
            (SHELL, None, Some(command))
        }
        "write_stdin" => (TEXT, None, s("chars")),
        "Edit" => (DIFF, path, Some(edit_diff(input))),
        "MultiEdit" => {
            let edits = input
                .get("edits")
                .and_then(Value::as_array)
                .map(|edits| edits.iter().map(edit_diff).collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            (DIFF, path, Some(edits))
        }
        "Write" => (TEXT, path, s("content")),
        "NotebookEdit" => (TEXT, path, s("new_source")),
        "apply_patch" => (
            DIFF,
            None,
            input
                .as_str()
                .map(str::to_owned)
                .or_else(|| s("input"))
                .or_else(|| s("patch")),
        ),
        "Read" => (TEXT, path, None),
        "Task" | "Agent" => (TEXT, None, s("prompt")),
        _ => match input {
            Value::Null => (JSON, path, None),
            Value::String(text) => (TEXT, path, Some(text.clone())),
            Value::Object(map) if map.is_empty() => (JSON, path, None),
            other => (JSON, path, Some(other.to_string())),
        },
    }
}

fn command_text(command: &Value) -> String {
    match command {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            if let [shell, flag, script] = words.as_slice()
                && shell.ends_with("sh")
                && flag.starts_with('-')
                && flag.ends_with('c')
            {
                return (*script).to_owned();
            }
            words
                .iter()
                .map(|word| {
                    if word.is_empty() || word.contains(char::is_whitespace) {
                        format!("'{}'", word.replace('\'', r"'\''"))
                    } else {
                        (*word).to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => String::new(),
    }
}

fn edit_diff(edit: &Value) -> String {
    let side = |key: &str, mark: char| {
        edit.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .lines()
            .map(|line| format!("{mark}{line}"))
            .collect::<Vec<_>>()
    };
    let mut lines = side("old_string", '-');
    lines.extend(side("new_string", '+'));
    lines.join("\n")
}

/// Claude Code's failed Bash result opens with `Exit code N`.
fn claude_exit_code(text: &str) -> Option<i64> {
    text.lines().next()?.strip_prefix("Exit code ")?.trim().parse().ok()
}

fn settle(call: &mut ToolCall, output: &str, exit: Option<i64>, status: Option<&'static str>) {
    if !output.is_empty() {
        call.output = Some(clip(output, MAX_TOOL_OUTPUT_BYTES));
    }
    call.exit_code = exit;
    call.status = status;
}

// ---------------------------------------------------------------------------------------------
// The colony stream (the collector's `colonizer.rs`)

/// A colony's event logs in run order: `events-1.jsonl`, `events-2.jsonl`, … then `events.jsonl`.
pub(crate) fn order_event_files(names: &[String]) -> Vec<String> {
    let mut archived: Vec<(u64, String)> = names
        .iter()
        .filter_map(|n| {
            Some((
                n.strip_prefix("events-")?.strip_suffix(".jsonl")?.parse::<u64>().ok()?,
                n.clone(),
            ))
        })
        .collect();
    archived.sort();
    let mut out: Vec<String> = archived.into_iter().map(|(_, n)| n).collect();
    if names.iter().any(|n| n == "events.jsonl") {
        out.push("events.jsonl".into());
    }
    out
}

/// One file's events. `seq` restarts per file; a line at or below the last `seq` seen in the same
/// file is agentd's re-send after a reconnect, and is dropped.
pub(crate) fn parse_events(bytes: &[u8]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut last_seq = 0u64;
    for line in bytes.split(|b| *b == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if let Some(seq) = value.get("seq").and_then(Value::as_u64) {
            if seq <= last_seq {
                continue;
            }
            last_seq = seq;
        }
        out.push(value);
    }
    out
}

/// The colony's row as `sessions.json` holds it.
pub(crate) fn row(s: &Session) -> Value {
    serde_json::to_value(s).unwrap_or(Value::Null)
}

/// The unredacted, uncapped record of one colony: its row and its events, in run order.
pub(crate) fn collect(colony: &Value, events: &[Value]) -> Option<SessionRecord> {
    let id = str_field(colony, "id");
    let repo = str_field(colony, "repo");
    if id.is_empty() || repo.is_empty() {
        return None;
    }
    let mut record = SessionRecord {
        schema_version: SCHEMA_VERSION,
        agent: AGENT,
        collector_version: COLLECTOR_VERSION,
        detail: Detail::Full,
        session_id: id.to_owned(),
        repo: repo.to_owned(),
        branch: opt(colony, "branch"),
        models: Vec::new(),
        started_at: opt(colony, "created_at"),
        ended_at: None,
        turns: Vec::new(),
        totals: Totals::default(),
        outcome: opt(colony, "status"),
        truncated: false,
        omitted_turns: 0,
        omitted_subagent_turns: 0,
        colony: Colony {
            org: repo.split('/').next().unwrap_or_default().to_owned(),
            base: opt(colony, "base"),
            issue: colony.get("issue").and_then(Value::as_u64),
            issue_title: opt(colony, "issue_title"),
            pr_url: opt(colony, "pr_url"),
            pr_opened_at: opt(colony, "pr_opened_at"),
            merged_at: opt(colony, "merged_at"),
            ci_state: opt(colony, "ci_state"),
            origin: opt(colony, "origin"),
        },
    };
    let mut stream = Stream::default();
    for event in events {
        stream.event(&mut record, colony, event);
    }
    stream.nest(&mut record);
    settle_models(&mut record.turns);
    if let Some(usage) = colony.get("model_usage").and_then(Value::as_object) {
        let sorted: BTreeMap<&String, &Value> = usage.iter().collect();
        for (model, tokens) in sorted {
            note_model(&mut record, model);
            record.totals.input_tokens += num_field(tokens, "input_tokens");
            record.totals.output_tokens += num_field(tokens, "output_tokens");
            record.totals.cache_read_tokens += num_field(tokens, "cache_read_tokens");
            record.totals.cache_creation_tokens += num_field(tokens, "cache_write_tokens");
        }
    }
    let costs: Vec<f64> = ["cost_usd", "routed_cost_usd"]
        .iter()
        .filter_map(|key| colony.get(*key).and_then(Value::as_f64))
        .collect();
    if !costs.is_empty() {
        record.totals.cost_usd = Some(costs.iter().sum());
    }
    Some(record)
}

/// Where a call sits: in the orchestrator's turns (`None`) or a subagent's (the id of the call that
/// started it), and at which turn and call.
type Place = (Option<String>, usize, usize);

/// One subagent's steps as the stream builds them, keyed by the call that started it.
#[derive(Default)]
struct Lane {
    name: String,
    description: Option<String>,
    /// The model the launch asked for (`haiku`, `sonnet`, …), when it named one: the fallback when
    /// none of its messages said which model it ran on.
    asked_model: Option<String>,
    models: Vec<String>,
    /// The model its latest message ran on.
    model: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    status: Option<&'static str>,
    turns: Vec<Turn>,
    opened: BTreeMap<String, usize>,
    /// Its last words: a background subagent's call output.
    said: String,
}

impl Lane {
    /// A lane for the call `id` launched with `input` (`Task`/`Agent`): named as the runner names the
    /// subagent, `subagent_type`, else its description.
    fn launched(input: &Value, at: &str) -> Lane {
        let description = opt(input, "description");
        let name = opt(input, "subagent_type")
            .or_else(|| description.clone())
            .unwrap_or_else(|| "subagent".to_owned());
        Lane {
            name,
            description,
            asked_model: opt(input, "model"),
            started_at: present(at),
            ..Lane::default()
        }
    }

    fn touch(&mut self, at: &str) {
        if at.is_empty() {
            return;
        }
        if self.started_at.is_none() {
            self.started_at = Some(at.to_owned());
        }
        self.ended_at = Some(at.to_owned());
    }

    fn into_subagent(mut self, id: &str) -> Subagent {
        if self.models.is_empty()
            && let Some(asked) = self.asked_model.take()
        {
            self.models.push(asked);
        }
        Subagent {
            id: id.to_owned(),
            name: self.name,
            description: self.description,
            models: self.models,
            started_at: self.started_at,
            ended_at: self.ended_at,
            status: self.status,
            totals: Totals::default(),
            turns: self.turns,
            truncated: false,
            omitted_turns: 0,
        }
    }
}

/// The status a subagent ended with, in the record's words.
fn subagent_status(word: &str) -> &'static str {
    match word {
        "completed" => "completed",
        "stopped" => "stopped",
        _ => "failed",
    }
}

#[derive(Default)]
struct Stream {
    started: bool,
    /// The orchestrator's open assistant turns, by message id.
    opened: BTreeMap<String, usize>,
    /// The orchestrator's model, as its last `model_changed` named it.
    model: Option<String>,
    calls: BTreeMap<String, Place>,
    lanes: BTreeMap<String, Lane>,
    /// Lane ids in the order they were opened: a subagent's own subagents open after it.
    order: Vec<String>,
}

/// The assistant turn a message id's blocks land in, in `turns`: the one already opened for it,
/// else a new one on `model`.
fn assistant_turn_in(
    turns: &mut Vec<Turn>,
    opened: &mut BTreeMap<String, usize>,
    message_id: &str,
    at: &str,
    model: Option<&String>,
) -> usize {
    if let Some(index) = opened.get(message_id) {
        return *index;
    }
    let mut turn = Turn::new(Role::Assistant, at, "");
    turn.model = model.cloned();
    turns.push(turn);
    let index = turns.len() - 1;
    if !message_id.is_empty() {
        opened.insert(message_id.to_owned(), index);
    }
    index
}

impl Stream {
    /// The lane a subagent event belongs to, opened from its `agent` when no launch was seen. Notes
    /// the model the event's agent ran on.
    fn lane(&mut self, event: &Value) -> Option<String> {
        let agent = event.get("agent")?;
        let id = str_field(agent, "id");
        if id.is_empty() {
            return None;
        }
        if !self.lanes.contains_key(id) {
            let name = opt(agent, "name").unwrap_or_else(|| "subagent".to_owned());
            self.lanes.insert(
                id.to_owned(),
                Lane {
                    name,
                    description: opt(agent, "description"),
                    ..Lane::default()
                },
            );
            self.order.push(id.to_owned());
        }
        let lane = self.lanes.get_mut(id).expect("just ensured");
        let model = str_field(agent, "model");
        if !model.is_empty() {
            lane.model = Some(model.to_owned());
            if !lane.models.iter().any(|seen| seen == model) {
                lane.models.push(model.to_owned());
            }
        }
        Some(id.to_owned())
    }

    /// The turns a call's place names.
    fn turns_at<'a>(&'a mut self, record: &'a mut SessionRecord, owner: &Option<String>) -> Option<&'a mut Vec<Turn>> {
        match owner {
            None => Some(&mut record.turns),
            Some(lane) => self.lanes.get_mut(lane).map(|lane| &mut lane.turns),
        }
    }

    /// The agent's (or a subagent's) text, thinking or tool call.
    fn step(&mut self, record: &mut SessionRecord, event: &Value, owner: Option<String>, at: &str) {
        note_time(record, at);
        let kind = str_field(event, "type");
        let (turns, opened, model) = match &owner {
            None => (&mut record.turns, &mut self.opened, self.model.as_ref()),
            Some(id) => {
                let Some(lane) = self.lanes.get_mut(id) else { return };
                lane.touch(at);
                (&mut lane.turns, &mut lane.opened, lane.model.as_ref())
            }
        };
        match kind {
            "assistant_text" => {
                let text = str_field(event, "text");
                if text.trim().is_empty() {
                    return;
                }
                let index = assistant_turn_in(turns, opened, str_field(event, "message_id"), at, model);
                let turn = &mut turns[index];
                if !turn.text.is_empty() {
                    turn.text.push('\n');
                }
                turn.text.push_str(text);
                if let Some(id) = &owner
                    && let Some(lane) = self.lanes.get_mut(id)
                {
                    lane.said = text.trim().to_owned();
                }
            }
            "thinking" => {
                let text = str_field(event, "text").trim();
                if !text.is_empty() {
                    turns.push(Turn::new(Role::Reasoning, at, text));
                }
            }
            "tool_call" => {
                let name = str_field(event, "name");
                if name.is_empty() {
                    return;
                }
                let index = assistant_turn_in(turns, opened, str_field(event, "message_id"), at, model);
                let turn = &mut turns[index];
                if turn.tool_calls.len() >= MAX_TOOL_CALLS {
                    return;
                }
                let input = event.get("input").unwrap_or(&Value::Null);
                turn.tool_calls.push(describe(name, input));
                let call = turn.tool_calls.len() - 1;
                let id = str_field(event, "tool_call_id");
                if id.is_empty() {
                    return;
                }
                self.calls.insert(id.to_owned(), (owner, index, call));
                if matches!(name, "Task" | "Agent") && !self.lanes.contains_key(id) {
                    self.lanes.insert(id.to_owned(), Lane::launched(input, at));
                    self.order.push(id.to_owned());
                }
            }
            _ => {}
        }
    }

    /// A tool's result, settled on its call wherever the call sits. A launch answered in the
    /// foreground ends its subagent with it.
    fn result(&mut self, record: &mut SessionRecord, event: &Value, at: &str) {
        let id = str_field(event, "tool_call_id");
        let Some((owner, turn, call)) = self.calls.get(id).cloned() else {
            return;
        };
        note_time(record, at);
        if let Some(owner) = &owner
            && let Some(lane) = self.lanes.get_mut(owner)
        {
            lane.touch(at);
        }
        if event.get("background").and_then(Value::as_bool) == Some(true) {
            return; // a background launch's ack: the call settles on its subagent_end
        }
        let output = str_field(event, "output");
        let failed = event.get("is_error").and_then(Value::as_bool) == Some(true);
        let exit = if failed { claude_exit_code(output) } else { None };
        if let Some(turns) = self.turns_at(record, &owner) {
            settle(
                &mut turns[turn].tool_calls[call],
                output,
                exit,
                Some(if failed { "error" } else { "ok" }),
            );
        }
        if let Some(lane) = self.lanes.get_mut(id) {
            lane.touch(at);
            lane.status = Some(if failed { "failed" } else { "completed" });
        }
    }

    /// A background subagent's end: its last words become its call's output.
    fn subagent_end(&mut self, record: &mut SessionRecord, event: &Value, at: &str) {
        let id = str_field(event, "tool_call_id");
        let Some((owner, turn, call)) = self.calls.get(id).cloned() else {
            return;
        };
        note_time(record, at);
        let word = str_field(event, "status");
        let status = match word {
            "completed" => "ok",
            "stopped" => "interrupted",
            _ => "error",
        };
        let said = match self.lanes.get_mut(id) {
            Some(lane) => {
                lane.touch(at);
                lane.status = Some(subagent_status(word));
                std::mem::take(&mut lane.said)
            }
            None => String::new(),
        };
        if let Some(turns) = self.turns_at(record, &owner) {
            settle(&mut turns[turn].tool_calls[call], &said, None, Some(status));
        }
    }

    /// Every lane onto the call that started it, a subagent's own subagents first — a lane opens
    /// after the lane its launch sits in, so the newest goes first. A lane whose call the record
    /// does not hold (past the per-turn cap, or before the logs begin) is dropped.
    fn nest(&mut self, record: &mut SessionRecord) {
        let order = std::mem::take(&mut self.order);
        for id in order.iter().rev() {
            let Some(lane) = self.lanes.remove(id) else { continue };
            let Some((owner, turn, call)) = self.calls.get(id).cloned() else {
                continue;
            };
            let subagent = lane.into_subagent(id);
            if let Some(turns) = self.turns_at(record, &owner) {
                turns[turn].tool_calls[call].subagent = Some(Box::new(subagent));
            }
        }
    }

    fn event(&mut self, record: &mut SessionRecord, colony: &Value, event: &Value) {
        let at = str_field(event, "ts");
        let origin = str_field(event, "origin");
        match (str_field(event, "type"), origin) {
            ("user_message", _) => {
                note_time(record, at);
                self.user_message(record, colony, event, at);
            }
            ("assistant_text" | "thinking" | "tool_call", "agent") => self.step(record, event, None, at),
            ("assistant_text" | "thinking" | "tool_call", "subagent") => {
                if let Some(lane) = self.lane(event) {
                    self.step(record, event, Some(lane), at);
                }
            }
            ("tool_result", "agent" | "subagent") => {
                if origin == "subagent" {
                    self.lane(event);
                }
                self.result(record, event, at);
            }
            ("subagent_end", _) => self.subagent_end(record, event, at),
            ("question", "agent") => {
                let text = question_text(event);
                if !text.is_empty() {
                    note_time(record, at);
                    record.turns.push(Turn::new(Role::Assistant, at, &text));
                }
            }
            ("question_answered", _) => {
                let answer = answer_text(event);
                if answer.is_empty() {
                    return;
                }
                note_time(record, at);
                if origin == "user" {
                    record.turns.push(Turn::new(Role::User, at, &answer));
                } else {
                    push_event(record, at, &format!("Answered automatically: {}", one_line(&answer, 300)));
                }
            }
            ("model_changed", _) => {
                let model = str_field(event, "model");
                note_model(record, model);
                if !model.is_empty() {
                    self.model = Some(model.to_owned());
                }
            }
            ("turn_end", _) if event.get("is_error").and_then(Value::as_bool) == Some(true) => {
                note_time(record, at);
                let result = one_line(str_field(event, "result"), 200);
                let text = if result.is_empty() {
                    "The turn ended in an error".to_owned()
                } else {
                    format!("The turn ended in an error: {result}")
                };
                push_event(record, at, &text);
            }
            ("verification", "system") => {
                let verdict = str_field(event, "verdict");
                if verdict.is_empty() {
                    return;
                }
                note_time(record, at);
                let summary = one_line(str_field(event, "summary"), 300);
                let text = if summary.is_empty() {
                    format!("Verification: {verdict}")
                } else {
                    format!("Verification: {verdict} — {summary}")
                };
                push_event(record, at, &text);
            }
            ("finding", _) => {
                note_time(record, at);
                push_event(
                    record,
                    at,
                    &format!("Finding filed: {}", one_line(str_field(event, "title"), 200)),
                );
            }
            ("loop_stop", _) => {
                note_time(record, at);
                let reason = one_line(str_field(event, "reason"), 300);
                push_event(record, at, &format!("Loop stopped: {reason}"));
            }
            _ => {}
        }
    }

    fn user_message(&mut self, record: &mut SessionRecord, colony: &Value, event: &Value, at: &str) {
        let id = str_field(event, "id");
        let origin = str_field(event, "origin");
        if id == "initial" {
            let first = !self.started;
            self.started = true;
            if origin == "redteam" {
                push_event(record, at, "Red-team probe started");
                return;
            }
            if !first {
                let text = if str_field(colony, "origin") == "redteam" {
                    "Red-team probe started"
                } else {
                    "Colony resumed"
                };
                push_event(record, at, text);
                return;
            }
            let instructions = str_field(colony, "instructions").trim();
            if !instructions.is_empty() {
                record.turns.push(Turn::new(Role::User, at, instructions));
                return;
            }
            let text = match (
                colony.get("issue").and_then(Value::as_u64),
                str_field(colony, "issue_title").trim(),
            ) {
                (Some(issue), "") => format!("Colony started on issue #{issue}"),
                (Some(issue), title) => format!("Colony started on issue #{issue}: {title}"),
                (None, _) => "Colony started".to_owned(),
            };
            push_event(record, at, &text);
            return;
        }
        if let Some(nudge) = nudge(id, origin) {
            push_event(record, at, nudge);
            return;
        }
        let text = str_field(event, "text").trim();
        if origin == "user" && !text.is_empty() {
            record.turns.push(Turn::new(Role::User, at, text));
        }
    }
}

/// A message the harness sent on its own account, by id prefix or a non-person origin.
fn nudge(id: &str, origin: &str) -> Option<&'static str> {
    const NUDGES: &[(&str, &str)] = &[
        ("watchdog-", "Watchdog recovery check"),
        (
            "verify-fix-",
            "Verification failed in a fresh checkout; a fix round was requested",
        ),
        ("pr-rewrite-", "Asked to rewrite the pull request description"),
        ("playbook-", "Playbook guidance from the harness"),
        ("u-selfheal-", "Continued after a provider error"),
        ("u-retry-", "Continued after a provider error"),
        ("u-quota-reset-", "Continued after the model quota reset"),
    ];
    if let Some((_, text)) = NUDGES.iter().find(|(prefix, _)| id.starts_with(prefix)) {
        return Some(text);
    }
    match origin {
        "user" => None,
        "watchdog" => Some("Watchdog recovery check"),
        "redteam" => Some("Red-team probe message"),
        _ => Some("Harness message"),
    }
}

fn question_text(event: &Value) -> String {
    let mut lines: Vec<String> = Vec::new();
    for question in event.get("questions").and_then(Value::as_array).into_iter().flatten() {
        let asked = match str_field(question, "question") {
            "" => str_field(question, "header"),
            asked => asked,
        };
        if !asked.trim().is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(asked.trim().to_owned());
        }
        for option in question.get("options").and_then(Value::as_array).into_iter().flatten() {
            let label = str_field(option, "label").trim();
            let description = str_field(option, "description").trim();
            match (label, description) {
                ("", "") => {}
                (label, "") => lines.push(format!("- {label}")),
                ("", description) => lines.push(format!("- {description}")),
                (label, description) => lines.push(format!("- {label}: {description}")),
            }
        }
    }
    lines.join("\n")
}

fn answer_text(event: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    match event.get("answers") {
        Some(Value::Object(answers)) => {
            let sorted: BTreeMap<&String, &Value> = answers.iter().collect();
            for answer in sorted.values() {
                match answer {
                    Value::String(text) if !text.trim().is_empty() => parts.push(text.trim().to_owned()),
                    Value::Array(choices) => {
                        let picked: Vec<&str> = choices
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::trim)
                            .filter(|t| !t.is_empty())
                            .collect();
                        if !picked.is_empty() {
                            parts.push(picked.join(", "));
                        }
                    }
                    _ => {}
                }
            }
        }
        Some(Value::String(text)) if !text.trim().is_empty() => parts.push(text.trim().to_owned()),
        _ => {}
    }
    let response = str_field(event, "response").trim();
    if !response.is_empty() {
        parts.push(response.to_owned());
    }
    parts.join("\n")
}

fn one_line(text: &str, max: usize) -> String {
    let mut line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_text(&mut line, max);
    line
}

/// Keep an assistant turn's `model` only where it changes from the previous assistant turn's in the
/// same list — the first one names it, each switch after does — and the same in every subagent's
/// turns, each list on its own.
fn settle_models(turns: &mut [Turn]) {
    let mut previous: Option<String> = None;
    for turn in turns.iter_mut() {
        if turn.role == Role::Assistant && turn.model.is_some() {
            if turn.model == previous {
                turn.model = None;
            } else {
                previous.clone_from(&turn.model);
            }
        }
        for call in &mut turn.tool_calls {
            if let Some(subagent) = call.subagent.as_mut() {
                settle_models(&mut subagent.turns);
            }
        }
    }
}

/// Every turn of `turns` and of every subagent under them, depth first.
fn each_turn(turns: &mut [Turn], f: &mut dyn FnMut(&mut Turn)) {
    for turn in turns.iter_mut() {
        f(turn);
        for call in &mut turn.tool_calls {
            if let Some(subagent) = call.subagent.as_mut() {
                each_turn(&mut subagent.turns, f);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Detail, redaction and the size cap (the CLI's `prepare`)

/// What the redaction pass needs to know about this machine: the home prefix and the saved secrets.
#[derive(Default)]
pub(crate) struct Scrubber {
    home: Option<String>,
    secrets: Vec<String>,
}

impl Scrubber {
    pub(crate) fn new(home: Option<String>, mut secrets: Vec<String>) -> Self {
        secrets.retain(|s| s.trim().len() >= 8);
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Scrubber {
            home: home.filter(|h| h.len() > 1),
            secrets,
        }
    }

    fn elide(&self, text: &mut String) {
        if let Some(home) = &self.home
            && text.contains(home.as_str())
        {
            *text = text.replace(home.as_str(), "~");
        }
    }

    /// One string value, redacted on its own.
    pub(crate) fn redact(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), "[REDACTED:secret]");
            }
        }
        colonizer_redact::redact_text(&out).into_owned()
    }

    /// Both, for the note: elision then redaction.
    pub(crate) fn clean(&self, text: &str) -> String {
        let mut out = text.to_owned();
        self.elide(&mut out);
        self.redact(&out)
    }
}

fn apply_detail(record: &mut SessionRecord, detail: Detail) {
    record.detail = detail;
    if detail == Detail::Full {
        return;
    }
    detail_turns(&mut record.turns, detail);
}

/// One list of turns brought down to `detail`, and every subagent's under it: reasoning gone, tool
/// calls their names (a call keeps the subagent it started), and under `summary` every text — a
/// subagent's description too — cut to [`SUMMARY_TURN_CHARS`].
fn detail_turns(turns: &mut Vec<Turn>, detail: Detail) {
    turns.retain(|turn| turn.role != Role::Reasoning);
    for turn in turns.iter_mut() {
        for call in &mut turn.tool_calls {
            let mut subagent = call.subagent.take();
            if let Some(subagent) = subagent.as_mut() {
                detail_turns(&mut subagent.turns, detail);
                if detail == Detail::Summary
                    && let Some(description) = subagent.description.as_mut()
                {
                    truncate_text(description, SUMMARY_TURN_CHARS);
                }
            }
            *call = ToolCall {
                name: std::mem::take(&mut call.name),
                subagent,
                ..ToolCall::default()
            };
        }
        if detail == Detail::Summary {
            truncate_text(&mut turn.text, SUMMARY_TURN_CHARS);
        }
    }
}

fn content_strings(record: &mut SessionRecord) -> Vec<&mut String> {
    let mut strings: Vec<&mut String> = vec![&mut record.repo];
    if let Some(branch) = record.branch.as_mut() {
        strings.push(branch);
    }
    if let Some(title) = record.colony.issue_title.as_mut() {
        strings.push(title);
    }
    turn_strings(&mut record.turns, &mut strings);
    strings
}

/// Every string a person, a tool or a subagent wrote in `turns`, subagents' descriptions and turns
/// included.
fn turn_strings<'a>(turns: &'a mut [Turn], strings: &mut Vec<&'a mut String>) {
    for turn in turns.iter_mut() {
        strings.push(&mut turn.text);
        for call in &mut turn.tool_calls {
            for field in [&mut call.path, &mut call.input, &mut call.output] {
                if let Some(value) = field.as_mut() {
                    strings.push(value);
                }
            }
            if let Some(subagent) = call.subagent.as_mut() {
                if let Some(description) = subagent.description.as_mut() {
                    strings.push(description);
                }
                turn_strings(&mut subagent.turns, strings);
            }
        }
    }
}

pub(crate) fn serialise(record: &SessionRecord) -> String {
    serde_json::to_string(record).expect("a session record always serialises")
}

fn escaped_len(text: &str) -> usize {
    serde_json::to_string(text).expect("a string always serialises").len() - 2
}

/// How far the record is over the caps, raw and escaped, with room for a marker turn; `None` when it
/// fits.
fn excess(record: &SessionRecord, marker_room: usize) -> Option<(isize, isize)> {
    let full = serialise(record);
    let (raw, escaped) = (full.len(), escaped_len(&full));
    if raw <= MAX_BODY_BYTES && escaped <= MAX_ESCAPED_BODY_BYTES {
        return None;
    }
    Some((
        (raw + marker_room) as isize - MAX_BODY_BYTES as isize,
        (escaped + 2 * marker_room) as isize - MAX_ESCAPED_BODY_BYTES as isize,
    ))
}

/// Shed a subagent's oldest turns until `excess` is paid or only its final answer (its last
/// assistant turn with text, else its last turn) and what follows it are left. The removed run
/// becomes one event turn. Returns how many turns went.
fn trim_subagent(subagent: &mut Subagent, excess: (isize, isize)) -> usize {
    let (mut excess_raw, mut excess_escaped) = excess;
    let Some(last) = subagent.turns.len().checked_sub(1) else {
        return 0;
    };
    let keep = subagent
        .turns
        .iter()
        .rposition(|turn| turn.role == Role::Assistant && !turn.text.trim().is_empty())
        .unwrap_or(last);
    let mut cut = 0;
    while (excess_raw > 0 || excess_escaped > 0) && cut < keep {
        let turn = serde_json::to_string(&subagent.turns[cut]).expect("a turn always serialises");
        excess_raw -= turn.len() as isize + 1;
        excess_escaped -= escaped_len(&turn) as isize + 1;
        cut += 1;
    }
    if cut == 0 {
        return 0;
    }
    let at = subagent.turns[..cut]
        .iter()
        .find_map(|turn| turn.at.clone())
        .unwrap_or_default();
    subagent.turns.drain(..cut);
    subagent.turns.insert(
        0,
        Turn::new(
            Role::Event,
            &at,
            &format!("{cut} earlier turn(s) of this subagent omitted to fit the upload size cap"),
        ),
    );
    subagent.truncated = true;
    subagent.omitted_turns += cut;
    cut
}

/// Bound the record for the wire. Subagents go first — the oldest first, each down to its final
/// answer — then the orchestrator's own turns after its opening one, then every text and tool
/// payload. Deterministic: the same colony caps to the same bytes.
fn cap(record: &mut SessionRecord) {
    const MARKER_ROOM: usize = 256;
    if excess(record, 0).is_none() {
        return;
    }
    let launches: Vec<(usize, usize)> = record
        .turns
        .iter()
        .enumerate()
        .flat_map(|(t, turn)| {
            turn.tool_calls
                .iter()
                .enumerate()
                .filter(|(_, call)| call.subagent.is_some())
                .map(move |(c, _)| (t, c))
        })
        .collect();
    for (t, c) in launches {
        let Some(over) = excess(record, MARKER_ROOM) else { break };
        let subagent = record.turns[t].tool_calls[c].subagent.as_mut().expect("a launch");
        let removed = trim_subagent(subagent, over);
        if removed > 0 {
            record.truncated = true;
            record.omitted_subagent_turns += removed;
        }
    }
    let full = serialise(record);
    let (raw, escaped) = (full.len(), escaped_len(&full));
    if raw <= MAX_BODY_BYTES && escaped <= MAX_ESCAPED_BODY_BYTES {
        return;
    }
    let mut excess_raw = (raw + MARKER_ROOM) as isize - MAX_BODY_BYTES as isize;
    let mut excess_escaped = (escaped + 2 * MARKER_ROOM) as isize - MAX_ESCAPED_BODY_BYTES as isize;
    let keep = usize::from(record.turns.len() > 1);
    let mut cut = keep;
    while (excess_raw > 0 || excess_escaped > 0) && cut < record.turns.len() {
        let turn = serde_json::to_string(&record.turns[cut]).expect("a turn always serialises");
        excess_raw -= turn.len() as isize + 1;
        excess_escaped -= escaped_len(&turn) as isize + 1;
        cut += 1;
    }
    let omitted = cut - keep;
    if omitted > 0 {
        let at = record.turns[keep..cut]
            .iter()
            .find_map(|turn| turn.at.clone())
            .unwrap_or_default();
        record.turns.drain(keep..cut);
        record.turns.insert(
            keep,
            Turn::new(
                Role::Event,
                &at,
                &format!("{omitted} earlier turn(s) omitted to fit the upload size cap"),
            ),
        );
    }
    record.truncated = true;
    record.omitted_turns += omitted;
    let body = serialise(record);
    if body.len() > MAX_BODY_BYTES || escaped_len(&body) > MAX_ESCAPED_BODY_BYTES {
        each_turn(&mut record.turns, &mut |turn| {
            truncate_text(&mut turn.text, SUMMARY_TURN_CHARS);
            for call in &mut turn.tool_calls {
                call.input = None;
                call.output = None;
            }
        });
    }
}

fn repair_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') => {
                out.push('\\');
                out.push(chars.next().expect("peeked"));
            }
            _ => out.push_str("\\\\"),
        }
    }
    out
}

/// The record at `detail`, home-elided, clipped, redacted value by value and capped — in the
/// CLI's order — and the body to send, checked once more as a whole.
pub(crate) fn prepare(mut record: SessionRecord, detail: Detail, scrub: &Scrubber) -> (SessionRecord, String) {
    apply_detail(&mut record, detail);
    for text in content_strings(&mut record) {
        scrub.elide(text);
    }
    each_turn(&mut record.turns, &mut |turn| {
        if turn.text.len() > MAX_TEXT_BYTES {
            turn.text = clip(&turn.text, MAX_TEXT_BYTES);
        }
    });
    for text in content_strings(&mut record) {
        if !text.is_empty() {
            *text = scrub.redact(text);
        }
    }
    cap(&mut record);
    let body = serialise(&record);
    let checked = scrub.redact(&body);
    let body = if checked == body {
        body
    } else {
        let parses = |text: &str| serde_json::from_str::<Value>(text).is_ok();
        if parses(&checked) {
            checked
        } else {
            let repaired = repair_escapes(&checked);
            if parses(&repaired) { repaired } else { checked }
        }
    };
    (record, body)
}

/// The source's filing name: `colonizer session <id>`.
pub(crate) fn source_path(record: &SessionRecord) -> String {
    format!("{AGENT} session {}", record.session_id)
}

/// The project a colony files under: its repository's name, cut to the brain's project rule.
pub(crate) fn project_of(repo: &str) -> Option<String> {
    let base = repo.rsplit(['/', '\\']).find(|seg| !seg.is_empty() && *seg != "~")?;
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let project: String = trimmed.chars().take(64).collect();
    (!project.is_empty()).then_some(project)
}

/// `agent-log-colonizer-<id>`, on the brain's slug rule.
pub(crate) fn note_slug(session_id: &str) -> String {
    let raw = format!("agent-log-{AGENT}-{session_id}").to_ascii_lowercase();
    let mut slug = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let ch = if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            ch
        } else {
            '-'
        };
        if ch == '-' && slug.ends_with('-') {
            continue;
        }
        slug.push(ch);
    }
    slug.truncate(128);
    slug.trim_end_matches('-').to_owned()
}
