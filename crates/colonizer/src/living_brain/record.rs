//! A colony as a Living Brain `agent_log` record and its note — the one place the mapping lives.
//!
//! The record is the normalised session `livingbrain logs sync` uploads for Claude Code and Codex
//! sessions (Livingbrain-wiki/livingbrain, `docs/logs.md` and `crates/livingbrain-cli/src/logs.rs`,
//! schema version 1), filled from what the mothership already keeps about a colony: its row in
//! `sessions.json` and its event logs (`events-<n>.jsonl`, then `events.jsonl`). Field order is wire
//! order and nothing here reads the clock or the environment, so one colony in one state is one
//! byte string — the property the brain's `(scope, sha256(body))` idempotency rests on.
//!
//! The `agent` is `colonizer`, the session id is the colony id, `repo` is `owner/name`, `outcome`
//! is the colony's status word. Issue, pull request, cost and the colony's own summary travel in the
//! note, which is free text; the record keeps the v1 shape byte for byte. Detail `full` adds each
//! tool call's input and output to the record, as the draft `full-session-transcripts` schema does —
//! that is a schema bump, so a full record says `schema_version: 2`. If the brain's final shape
//! differs, this file is the only one that changes.
//!
//! Redaction is not optional and happens here, on this machine, before anything serialises: the
//! mothership's home directory becomes `~`, every secret value the Secrets page holds becomes
//! `[REDACTED:secret]`, and then the shared detectors (`colonizer-redact`) run over every free-text
//! field. The brain redacts again on arrival.

use crate::sessions::Session;
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// The normalised session schema this collector writes for `messages` and `summary`.
pub(crate) const SCHEMA_VERSION: u32 = 1;
/// The schema a `full` record declares: v1 plus tool inputs and outputs (the draft shape).
pub(crate) const FULL_SCHEMA_VERSION: u32 = 2;
/// The `agent` value, and half the source's filing name (`colonizer session <id>`).
pub(crate) const AGENT: &str = "colonizer";
/// This collector's own version: bumped when what it reads out of a colony changes.
pub(crate) const COLLECTOR_VERSION: u32 = 1;
/// The most text one turn keeps, in characters — the CLI's cap.
const MAX_TURN_TEXT_CHARS: usize = 2000;
/// The most tool calls one turn names.
const MAX_TOOL_CALLS: usize = 128;
/// `full` only: a tool input keeps its first and last 8 KiB, an output its first and last 2 KiB.
const MAX_TOOL_INPUT_BYTES: usize = 16 * 1024;
const MAX_TOOL_OUTPUT_BYTES: usize = 4 * 1024;
/// The largest body built: three quarters of the brain's 1 MiB redaction input cap, as the CLI.
pub(crate) const MAX_BODY_BYTES: usize = 1024 * 1024 * 3 / 4;
/// The note quotes each turn to this many characters, the whole excerpt to this many bytes.
const NOTE_TURN_CHARS: usize = 400;
const NOTE_EXCERPT_BYTES: usize = 12 * 1024;

/// How much of a colony travels, per repository (Settings → Living Brain).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Detail {
    /// The conversation, tool names, and every tool call's input and output (clipped).
    Full,
    /// The conversation and the names of the tools called — the CLI's record.
    #[default]
    Messages,
    /// Who, where, when, with what and at what cost; no conversation at all.
    Summary,
}

#[derive(Debug, Serialize)]
pub(crate) struct SessionRecord {
    schema_version: u32,
    agent: &'static str,
    collector_version: u32,
    pub(crate) session_id: String,
    pub(crate) repo: String,
    branch: Option<String>,
    models: Vec<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    turns: Vec<Turn>,
    totals: Totals,
    outcome: Option<String>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct Turn {
    role: String,
    at: Option<String>,
    text: String,
    tool_calls: Vec<ToolCall>,
}

/// A tool call: its name, and under `full` its input and output. Absent fields are omitted, so a
/// `messages` record's call is exactly v1's `{"name": …}`.
#[derive(Debug, Serialize)]
struct ToolCall {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

#[derive(Debug, Default, Serialize)]
struct Totals {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    reasoning_tokens: u64,
    cost_usd: Option<f64>,
}

/// What the redaction pass needs to know about this machine: the home prefix to elide and the
/// saved secret values to scrub. Built once per send.
#[derive(Default)]
pub(crate) struct Scrubber {
    home: Option<String>,
    secrets: Vec<String>,
}

impl Scrubber {
    pub(crate) fn new(home: Option<String>, mut secrets: Vec<String>) -> Self {
        // Longest first, so a secret that contains another is replaced whole; very short values
        // would shred ordinary prose and are not credentials anyway.
        secrets.retain(|s| s.trim().len() >= 8);
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Scrubber {
            home: home.filter(|h| h.len() > 1),
            secrets,
        }
    }

    /// One free-text field, made safe to leave the machine.
    pub(crate) fn clean(&self, text: &str) -> String {
        let mut out = text.to_owned();
        if let Some(home) = &self.home
            && out.contains(home.as_str())
        {
            out = out.replace(home.as_str(), "~");
        }
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), "[REDACTED:secret]");
            }
        }
        colonizer_redact::redact_text(&out).into_owned()
    }
}

/// The colony's task as its person gave it: the issue and the operator's instructions, never the
/// harness prompt the guest was booted with (that is boilerplate, as Codex's
/// `<user_instructions>` is to the CLI).
fn initial_task(s: &Session) -> String {
    let mut parts = Vec::new();
    if let Some(issue) = s.issue {
        if s.issue_title.trim().is_empty() {
            parts.push(format!("Issue #{issue}"));
        } else {
            parts.push(format!("Issue #{issue}: {}", s.issue_title.trim()));
        }
    }
    if !s.instructions.trim().is_empty() {
        parts.push(s.instructions.trim().to_owned());
    }
    parts.join("\n\n")
}

fn truncate_chars(text: &mut String, max: usize) {
    if let Some((cut, _)) = text.char_indices().nth(max) {
        text.truncate(cut);
    }
}

/// `text` clipped to `max` bytes keeping its head and tail, on char boundaries.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let half = max / 2;
    let mut head = half;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - half;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}\n… [{} bytes elided] …\n{}", &text[..head], tail - head, &text[tail..])
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The colony's event logs in the order they were written: the archives a resume rotated aside
/// (`events-1.jsonl`, `events-2.jsonl`, …) and then the live `events.jsonl`. Each file's lines are
/// replayed with the store's `seq` rule — a line at or below the last `seq` seen in that file is
/// agentd's re-send after a reconnect and is dropped.
pub(crate) fn order_event_files(names: &[String]) -> Vec<String> {
    let mut archived: Vec<(u64, String)> = names
        .iter()
        .filter_map(|n| {
            let k = n.strip_prefix("events-")?.strip_suffix(".jsonl")?.parse::<u64>().ok()?;
            Some((k, n.clone()))
        })
        .collect();
    archived.sort();
    let mut out: Vec<String> = archived.into_iter().map(|(_, n)| n).collect();
    if names.iter().any(|n| n == "events.jsonl") {
        out.push("events.jsonl".into());
    }
    out
}

/// The parsed events of one file, deduplicated by `seq`.
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

/// Builds the record for one colony from its row and its events, redacted, capped, in wire order.
pub(crate) fn build(s: &Session, events: &[Value], detail: Detail, scrub: &Scrubber) -> SessionRecord {
    let full = detail == Detail::Full;
    let mut models: Vec<String> = Vec::new();
    let note_model = |models: &mut Vec<String>, m: &str| {
        if !m.is_empty() && !models.iter().any(|seen| seen == m) {
            models.push(m.to_owned());
        }
    };
    let mut turns: Vec<Turn> = Vec::new();
    // message id → its turn; tool call id → (turn, call), for the full record's outputs.
    let mut by_message: HashMap<String, usize> = HashMap::new();
    let mut by_call: HashMap<String, (usize, usize)> = HashMap::new();
    let mut ended_at: Option<String> = None;
    for event in events {
        let at = event.get("ts").and_then(Value::as_str).map(str::to_owned);
        if at.is_some() {
            ended_at = at.clone();
        }
        // Subagents' own lines are their work, not the colony's conversation: the orchestrator's
        // tool call that started each one is what the record names.
        if event.get("agent").is_some_and(Value::is_object) {
            continue;
        }
        match str_of(event, "type") {
            "model_changed" => note_model(&mut models, str_of(event, "model")),
            "user_message" => {
                let id = str_of(event, "id");
                if id.starts_with("watchdog-") {
                    continue; // the harness nudging, not the person
                }
                let text = if id == "initial" {
                    initial_task(s)
                } else {
                    str_of(event, "text").to_owned()
                };
                turns.push(Turn {
                    role: "user".into(),
                    at,
                    text,
                    tool_calls: Vec::new(),
                });
            }
            "assistant_text" => {
                let message = str_of(event, "message_id").to_owned();
                let text = str_of(event, "text");
                match by_message.get(&message).copied() {
                    Some(i) if i + 1 == turns.len() => {
                        let turn = &mut turns[i];
                        if !turn.text.is_empty() {
                            turn.text.push_str("\n\n");
                        }
                        turn.text.push_str(text);
                    }
                    _ => {
                        turns.push(Turn {
                            role: "assistant".into(),
                            at,
                            text: text.to_owned(),
                            tool_calls: Vec::new(),
                        });
                        by_message.insert(message, turns.len() - 1);
                    }
                }
            }
            "tool_call" => {
                let message = str_of(event, "message_id").to_owned();
                let i = match by_message.get(&message).copied() {
                    Some(i) if i + 1 == turns.len() => i,
                    _ => {
                        turns.push(Turn {
                            role: "assistant".into(),
                            at,
                            text: String::new(),
                            tool_calls: Vec::new(),
                        });
                        by_message.insert(message, turns.len() - 1);
                        turns.len() - 1
                    }
                };
                let turn = &mut turns[i];
                if turn.tool_calls.len() >= MAX_TOOL_CALLS {
                    continue;
                }
                let input = full.then(|| match event.get("input") {
                    Some(Value::String(text)) => clip(text, MAX_TOOL_INPUT_BYTES),
                    Some(Value::Object(map)) if map.is_empty() => String::new(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => clip(&other.to_string(), MAX_TOOL_INPUT_BYTES),
                });
                turn.tool_calls.push(ToolCall {
                    name: str_of(event, "name").to_owned(),
                    input: input.filter(|text| !text.is_empty()),
                    output: None,
                    is_error: None,
                });
                by_call.insert(str_of(event, "tool_call_id").to_owned(), (i, turn.tool_calls.len() - 1));
            }
            "tool_result" if full => {
                if let Some(&(t, c)) = by_call.get(str_of(event, "tool_call_id"))
                    && let Some(call) = turns.get_mut(t).and_then(|turn| turn.tool_calls.get_mut(c))
                {
                    let output = str_of(event, "output");
                    call.output = (!output.is_empty()).then(|| clip(output, MAX_TOOL_OUTPUT_BYTES));
                    call.is_error = event.get("is_error").and_then(Value::as_bool);
                }
            }
            "question" => {
                let asked: Vec<&str> = event
                    .get("questions")
                    .and_then(Value::as_array)
                    .map(|qs| qs.iter().map(|q| str_of(q, "question")).filter(|q| !q.is_empty()).collect())
                    .unwrap_or_default();
                if !asked.is_empty() {
                    turns.push(Turn {
                        role: "assistant".into(),
                        at,
                        text: format!("Question: {}", asked.join(" / ")),
                        tool_calls: Vec::new(),
                    });
                }
            }
            "question_answered" => {
                let mut said: Vec<String> = event
                    .get("answers")
                    .and_then(Value::as_object)
                    .map(|answers| {
                        answers
                            .values()
                            .map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(response) = event.get("response").and_then(Value::as_str).filter(|r| !r.is_empty()) {
                    said.push(response.to_owned());
                }
                if !said.is_empty() {
                    turns.push(Turn {
                        role: "user".into(),
                        at,
                        text: format!("Answer: {}", said.join(" / ")),
                        tool_calls: Vec::new(),
                    });
                }
            }
            _ => {}
        }
    }

    // Totals from the colony's own cumulative usage (the last turn_end's `model_usage`, kept on the
    // row): every model counted once. Cost is the row's, the routed models' added in.
    let mut totals = Totals::default();
    if let Some(usage) = s.model_usage.as_ref().and_then(Value::as_object) {
        let sorted: BTreeMap<&String, &Value> = usage.iter().collect();
        for (model, u) in sorted {
            note_model(&mut models, model);
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            totals.input_tokens += n("input_tokens");
            totals.output_tokens += n("output_tokens");
            totals.cache_read_tokens += n("cache_read_tokens");
            totals.cache_creation_tokens += n("cache_write_tokens");
            totals.reasoning_tokens += n("reasoning_tokens");
        }
    }
    totals.cost_usd = match (s.cost_usd, s.routed_cost_usd) {
        (None, None) => None,
        (a, b) => Some(((a.unwrap_or(0.0) + b.unwrap_or(0.0)) * 1e6).round() / 1e6),
    };

    if detail == Detail::Summary {
        turns.clear();
    }
    for turn in &mut turns {
        truncate_chars(&mut turn.text, MAX_TURN_TEXT_CHARS);
        turn.text = scrub.clean(&turn.text);
        for call in &mut turn.tool_calls {
            call.name = scrub.clean(&call.name);
            call.input = call.input.as_deref().map(|t| scrub.clean(t));
            call.output = call.output.as_deref().map(|t| scrub.clean(t));
        }
    }
    let outcome = serde_json::to_value(s.status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned));
    let mut record = SessionRecord {
        schema_version: if full { FULL_SCHEMA_VERSION } else { SCHEMA_VERSION },
        agent: AGENT,
        collector_version: COLLECTOR_VERSION,
        session_id: s.id.clone(),
        repo: scrub.clean(&s.repo),
        branch: (!s.branch.is_empty()).then(|| scrub.clean(&s.branch)),
        models,
        started_at: Some(s.created_at.to_rfc3339_opts(SecondsFormat::Millis, true)),
        ended_at: ended_at.or_else(|| Some(s.updated_at.to_rfc3339_opts(SecondsFormat::Millis, true))),
        turns,
        totals,
        outcome,
        truncated: false,
    };
    cap(&mut record);
    record
}

/// Whole trailing turns off until the body fits [`MAX_BODY_BYTES`]; a record that lost any says so.
fn cap(record: &mut SessionRecord) {
    let full = serialise(record);
    if full.len() <= MAX_BODY_BYTES {
        return;
    }
    let mut excess = (full.len() - MAX_BODY_BYTES) as isize;
    while excess > 0 {
        let Some(turn) = record.turns.pop() else { break };
        excess -= serde_json::to_string(&turn).map(|s| s.len()).unwrap_or(0) as isize + 1;
        record.truncated = true;
    }
}

/// The record's bytes: declaration order, nothing from the clock.
pub(crate) fn serialise(record: &SessionRecord) -> String {
    serde_json::to_string(record).expect("a session record always serialises")
}

/// The source's filing name on the wire: `colonizer session <id>`.
pub(crate) fn source_path(record: &SessionRecord) -> String {
    format!("{AGENT} session {}", record.session_id)
}

/// The project a colony files under: its repository's name (the CLI files `~/code/demo` as `demo`,
/// so a repository's colonies and its local sessions meet in one project), cut to the brain's
/// project rule — ASCII letters, digits, `.`, `_`, `-`, starting alphanumeric, at most 64 bytes.
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

/// `agent-log-colonizer-<id>`, on the brain's slug rule — the CLI's `slug_for`.
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

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn excerpt(text: &str) -> String {
    let joined = one_line(text);
    let mut out: String = joined.chars().take(NOTE_TURN_CHARS).collect();
    if joined.chars().nth(NOTE_TURN_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// A space between two adjacent `[` or `]`, so nothing parses as a wiki-link (the CLI's rule).
fn defuse_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous = None;
    for ch in text.chars() {
        if matches!(ch, '[' | ']') && previous == Some(ch) {
            out.push(' ');
        }
        out.push(ch);
        previous = Some(ch);
    }
    out
}

/// The note's markdown: the CLI's note (`logs/note.rs` `render`) plus what only a colony has — the
/// issue, the pull request and the colony's status. Built from the already-redacted record and the
/// row, redacted again, links defused last.
pub(crate) fn note_body(record: &SessionRecord, s: &Session, source: Option<&str>, scrub: &Scrubber) -> String {
    let mut out = String::new();
    let project = project_of(&record.repo).unwrap_or_default();
    out.push_str(&format!("Coding agent session: {} in {project}.\n\n", record.agent));
    out.push_str(&format!("- Agent: {}\n", record.agent));
    out.push_str(&format!("- Session: {}\n", record.session_id));
    out.push_str(&format!("- Repo: {}\n", record.repo));
    if let Some(branch) = &record.branch {
        out.push_str(&format!("- Branch: {}\n", one_line(branch)));
    }
    if let Some(issue) = s.issue {
        let title = one_line(&s.issue_title);
        if title.is_empty() {
            out.push_str(&format!("- Issue: #{issue}\n"));
        } else {
            out.push_str(&format!("- Issue: #{issue} {title}\n"));
        }
    }
    if let Some(pr) = &s.pr_url {
        out.push_str(&format!("- Pull request: {pr}\n"));
    }
    if let Some(outcome) = &record.outcome {
        out.push_str(&format!("- Status: {outcome}\n"));
    }
    if let Some(summary) = s.summary.as_deref().map(one_line).filter(|t| !t.is_empty()) {
        out.push_str(&format!("- Summary: {summary}\n"));
    }
    match (&record.started_at, &record.ended_at) {
        (Some(start), Some(end)) if start != end => out.push_str(&format!("- When: {start} to {end}\n")),
        (Some(at), _) | (None, Some(at)) => out.push_str(&format!("- When: {at}\n")),
        (None, None) => {}
    }
    if !record.models.is_empty() {
        out.push_str(&format!("- Models: {}\n", record.models.join(", ")));
    }
    let users = record.turns.iter().filter(|t| t.role == "user").count();
    out.push_str(&format!(
        "- Turns: {} ({users} user, {} assistant)\n",
        record.turns.len(),
        record.turns.len() - users
    ));
    out.push_str(&format!("- Tool calls: {}\n", tool_summary(&record.turns)));
    let t = &record.totals;
    out.push_str(&format!(
        "- Tokens: {} in, {} out, {} cache read, {} cache write, {} reasoning\n",
        t.input_tokens, t.output_tokens, t.cache_read_tokens, t.cache_creation_tokens, t.reasoning_tokens
    ));
    if let Some(cost) = t.cost_usd {
        out.push_str(&format!("- Cost: ${cost:.4}\n"));
    }
    if let Some(source) = source.filter(|s| !s.is_empty()) {
        out.push_str(&format!("- Source: {source} (agent_log)\n"));
    }
    if record.truncated {
        out.push_str("- Truncated: the record lost trailing turns to the size cap\n");
    }
    out.push_str("\n## Conversation\n\n");
    let spoken: Vec<&Turn> = record.turns.iter().filter(|t| !t.text.trim().is_empty()).collect();
    let mut used = 0usize;
    let mut shown = 0usize;
    for turn in &spoken {
        let speaker = if turn.role == "user" { "User" } else { "Assistant" };
        let line = format!("**{speaker}:** {}\n\n", excerpt(&turn.text));
        if used + line.len() > NOTE_EXCERPT_BYTES {
            break;
        }
        used += line.len();
        shown += 1;
        out.push_str(&line);
    }
    if spoken.is_empty() {
        out.push_str("_No text turns._\n");
    } else if shown < spoken.len() {
        out.push_str(&format!(
            "_{} more turn(s) not shown; the full record is the source._\n",
            spoken.len() - shown
        ));
    }
    defuse_links(&scrub.clean(&out))
}

fn tool_summary(turns: &[Turn]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for call in turns.iter().flat_map(|t| &t.tool_calls) {
        *counts.entry(call.name.as_str()).or_default() += 1;
    }
    let total: usize = counts.values().sum();
    if total == 0 {
        return "0".to_owned();
    }
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let names: Vec<String> = ranked.iter().map(|(n, c)| format!("{} ×{c}", one_line(n))).collect();
    format!("{total} ({})", names.join(", "))
}
