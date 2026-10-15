//! A colony as a Living Brain `agent_log` record — the one place the mapping lives.
//!
//! The record is the brain's normalised session, schema version 2 (Livingbrain-wiki/livingbrain,
//! `docs/logs.md` and `crates/livingbrain-cli/src/logs.rs`), with the `colonizer` collector's
//! mapping: one record per colony, read from its `sessions.json` row (serialised exactly as the
//! store writes it) and its event logs (`events-1.jsonl` … then `events.jsonl`). Field order is wire
//! order and nothing reads the clock or the environment, so one colony in one state is one byte
//! string — what the brain's `(scope, sha256(body))` idempotency rests on.
//!
//! What a record keeps of the stream: the maintainer's instructions as the opening user turn (the
//! harness prompt around them is boilerplate), the person's own messages and answers, the
//! orchestrating agent's text, thinking and tool calls with their results, and the colony's
//! milestones as `event` turns. Harness nudges become one-line events. A background subagent is
//! folded into the call that started it: its last words are that call's output.
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

pub(crate) const SCHEMA_VERSION: u32 = 2;
pub(crate) const AGENT: &str = "colonizer";
pub(crate) const COLLECTOR_VERSION: u32 = 1;
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
    pub(crate) text: String,
    pub(crate) tool_calls: Vec<ToolCall>,
}

impl Turn {
    fn new(role: Role, at: &str, text: &str) -> Self {
        Turn {
            role,
            at: present(at),
            text: text.to_owned(),
            tool_calls: Vec::new(),
        }
    }
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

#[derive(Default)]
struct Stream {
    started: bool,
    opened: BTreeMap<String, usize>,
    calls: BTreeMap<String, (usize, usize)>,
    subagent_said: BTreeMap<String, String>,
}

impl Stream {
    fn event(&mut self, record: &mut SessionRecord, colony: &Value, event: &Value) {
        let at = str_field(event, "ts");
        let origin = str_field(event, "origin");
        match (str_field(event, "type"), origin) {
            ("user_message", _) => {
                note_time(record, at);
                self.user_message(record, colony, event, at);
            }
            ("assistant_text", "agent") => {
                let text = str_field(event, "text");
                if text.trim().is_empty() {
                    return;
                }
                note_time(record, at);
                let index = self.assistant_turn(record, str_field(event, "message_id"), at);
                let turn = &mut record.turns[index];
                if !turn.text.is_empty() {
                    turn.text.push('\n');
                }
                turn.text.push_str(text);
            }
            ("thinking", "agent") => {
                let text = str_field(event, "text").trim();
                if !text.is_empty() {
                    note_time(record, at);
                    record.turns.push(Turn::new(Role::Reasoning, at, text));
                }
            }
            ("tool_call", "agent") => {
                let name = str_field(event, "name");
                if name.is_empty() {
                    return;
                }
                note_time(record, at);
                let index = self.assistant_turn(record, str_field(event, "message_id"), at);
                let turn = &mut record.turns[index];
                if turn.tool_calls.len() >= MAX_TOOL_CALLS {
                    return;
                }
                turn.tool_calls
                    .push(describe(name, event.get("input").unwrap_or(&Value::Null)));
                let id = str_field(event, "tool_call_id");
                if !id.is_empty() {
                    self.calls.insert(id.to_owned(), (index, turn.tool_calls.len() - 1));
                }
            }
            ("tool_result", "agent") => {
                let Some(&(turn, call)) = self.calls.get(str_field(event, "tool_call_id")) else {
                    return;
                };
                note_time(record, at);
                if event.get("background").and_then(Value::as_bool) == Some(true) {
                    return; // a background launch's ack: the call settles on its subagent_end
                }
                let output = str_field(event, "output");
                let failed = event.get("is_error").and_then(Value::as_bool) == Some(true);
                let exit = if failed { claude_exit_code(output) } else { None };
                settle(
                    &mut record.turns[turn].tool_calls[call],
                    output,
                    exit,
                    Some(if failed { "error" } else { "ok" }),
                );
            }
            ("assistant_text", "subagent") => {
                let started_by = event.get("agent").map(|agent| str_field(agent, "id")).unwrap_or_default();
                let text = str_field(event, "text").trim();
                if !started_by.is_empty() && !text.is_empty() {
                    self.subagent_said.insert(started_by.to_owned(), text.to_owned());
                }
            }
            ("subagent_end", _) => {
                let id = str_field(event, "tool_call_id");
                let Some(&(turn, call)) = self.calls.get(id) else { return };
                note_time(record, at);
                let status = match str_field(event, "status") {
                    "completed" => "ok",
                    "stopped" => "interrupted",
                    _ => "error",
                };
                let said = self.subagent_said.remove(id).unwrap_or_default();
                settle(&mut record.turns[turn].tool_calls[call], &said, None, Some(status));
            }
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
            ("model_changed", _) => note_model(record, str_field(event, "model")),
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

    fn assistant_turn(&mut self, record: &mut SessionRecord, message_id: &str, at: &str) -> usize {
        if let Some(index) = self.opened.get(message_id) {
            return *index;
        }
        record.turns.push(Turn::new(Role::Assistant, at, ""));
        let index = record.turns.len() - 1;
        if !message_id.is_empty() {
            self.opened.insert(message_id.to_owned(), index);
        }
        index
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
    record.turns.retain(|turn| turn.role != Role::Reasoning);
    for turn in &mut record.turns {
        for call in &mut turn.tool_calls {
            *call = ToolCall {
                name: std::mem::take(&mut call.name),
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
    for turn in &mut record.turns {
        strings.push(&mut turn.text);
        for call in &mut turn.tool_calls {
            for field in [&mut call.path, &mut call.input, &mut call.output] {
                if let Some(value) = field.as_mut() {
                    strings.push(value);
                }
            }
        }
    }
    strings
}

pub(crate) fn serialise(record: &SessionRecord) -> String {
    serde_json::to_string(record).expect("a session record always serialises")
}

fn escaped_len(text: &str) -> usize {
    serde_json::to_string(text).expect("a string always serialises").len() - 2
}

fn cap(record: &mut SessionRecord) {
    let full = serialise(record);
    let (raw, escaped) = (full.len(), escaped_len(&full));
    if raw <= MAX_BODY_BYTES && escaped <= MAX_ESCAPED_BODY_BYTES {
        return;
    }
    const MARKER_ROOM: usize = 256;
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
        for turn in &mut record.turns {
            truncate_text(&mut turn.text, SUMMARY_TURN_CHARS);
            for call in &mut turn.tool_calls {
                call.input = None;
                call.output = None;
            }
        }
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
    for turn in &mut record.turns {
        if turn.text.len() > MAX_TEXT_BYTES {
            turn.text = clip(&turn.text, MAX_TEXT_BYTES);
        }
    }
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
