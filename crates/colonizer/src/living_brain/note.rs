//! The searchable side of a sent colony: one note per colony, keyed `agent-log-colonizer-<id>`, filed
//! under the repository's name — the brain CLI's session note (`crates/livingbrain-cli/src/logs/
//! note.rs`): a header (agent, session, repo, branch, issue, pull request, outcome, when, models,
//! counts, tokens, cost, detail, the source and a link to its transcript) and the conversation in
//! order, quoted to fit a 48 KiB page. Built from the already-redacted record, redacted again,
//! wiki-link brackets defused last.

use super::record::{Role, SessionRecord, ToolCall, Turn, clip};
use std::collections::BTreeMap;

const NOTE_BUDGET_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, Copy)]
struct Limits {
    text: usize,
    input: usize,
    output: usize,
}

const NOTE_LIMITS: [Limits; 5] = [
    Limits {
        text: 6 * 1024,
        input: 1536,
        output: 1024,
    },
    Limits {
        text: 4 * 1024,
        input: 1024,
        output: 512,
    },
    Limits {
        text: 3 * 1024,
        input: 768,
        output: 256,
    },
    Limits {
        text: 2 * 1024,
        input: 512,
        output: 128,
    },
    Limits {
        text: 2 * 1024,
        input: 384,
        output: 0,
    },
];

/// The note's markdown, redacted and defused.
pub(crate) fn body(record: &SessionRecord, source: Option<&str>, scrub: &super::record::Scrubber) -> String {
    defuse_links(&scrub.clean(&render(record, source)))
}

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

fn render(record: &SessionRecord, source: Option<&str>) -> String {
    let mut out = header(record, source);
    out.push_str("\n## Conversation\n\n");
    let budget = NOTE_BUDGET_BYTES.saturating_sub(escaped_len(&out));
    let mut blocks: Vec<String> = Vec::new();
    let mut sizes: Vec<usize> = Vec::new();
    for limits in NOTE_LIMITS {
        blocks = record
            .turns
            .iter()
            .map(|turn| turn_block(turn, limits))
            .filter(|b| !b.is_empty())
            .collect();
        sizes = blocks.iter().map(|b| escaped_len(b)).collect();
        if sizes.iter().sum::<usize>() <= budget {
            for block in &blocks {
                out.push_str(block);
            }
            if blocks.is_empty() {
                out.push_str("_No text turns._\n");
            }
            return out;
        }
    }
    let marker_room = 512;
    let head_budget = budget / 3;
    let mut used = 0usize;
    let mut head = 0usize;
    while head < blocks.len() && used + sizes[head] <= head_budget {
        used += sizes[head];
        head += 1;
    }
    let mut tail = blocks.len();
    while tail > head && used + sizes[tail - 1] + marker_room <= budget {
        used += sizes[tail - 1];
        tail -= 1;
    }
    for block in &blocks[..head] {
        out.push_str(block);
    }
    let hidden = tail - head;
    if hidden > 0 {
        let link = match source.filter(|s| !s.is_empty()) {
            Some(source) => format!("open the [full transcript](wiki.html?view=sessions&source={source}) in Agent sessions"),
            None => "the full transcript is in the session's source (Agent sessions)".to_owned(),
        };
        out.push_str(&format!("_… {hidden} turn(s) not shown on this page — {link}._\n\n"));
    }
    for block in &blocks[tail..] {
        out.push_str(block);
    }
    out
}

fn header(record: &SessionRecord, source: Option<&str>) -> String {
    let mut out = String::new();
    let project = super::record::project_of(&record.repo).unwrap_or_default();
    out.push_str(&format!("Coding agent session: {} in {project}.\n\n", record.agent));
    out.push_str(&format!("- Agent: {}\n", record.agent));
    out.push_str(&format!("- Session: {}\n", record.session_id));
    out.push_str(&format!("- Repo: {}\n", record.repo));
    if let Some(branch) = &record.branch {
        out.push_str(&format!("- Branch: {}\n", one_line(branch)));
    }
    let colony = &record.colony;
    match (colony.issue, &colony.issue_title) {
        (Some(issue), Some(title)) => out.push_str(&format!("- Issue: #{issue} {}\n", one_line(title))),
        (Some(issue), None) => out.push_str(&format!("- Issue: #{issue}\n")),
        _ => {}
    }
    if let Some(pr) = &colony.pr_url {
        out.push_str(&format!("- Pull request: {}\n", one_line(pr)));
    }
    if let Some(outcome) = &record.outcome {
        out.push_str(&format!("- Outcome: {}\n", one_line(outcome)));
    }
    match (&record.started_at, &record.ended_at) {
        (Some(start), Some(end)) if start != end => out.push_str(&format!("- When: {start} to {end}\n")),
        (Some(at), _) | (None, Some(at)) => out.push_str(&format!("- When: {at}\n")),
        (None, None) => {}
    }
    if !record.models.is_empty() {
        out.push_str(&format!("- Models: {}\n", record.models.join(", ")));
    }
    let users = record.turns.iter().filter(|t| t.role == Role::User).count();
    let said = record
        .turns
        .iter()
        .filter(|t| t.role == Role::Assistant && !t.text.trim().is_empty())
        .count();
    out.push_str(&format!("- Messages: {users} user, {said} assistant\n"));
    out.push_str(&format!("- Tool calls: {}\n", tool_summary(&record.turns)));
    let t = &record.totals;
    out.push_str(&format!(
        "- Tokens: {} in, {} out, {} cache read, {} cache write, {} reasoning\n",
        t.input_tokens, t.output_tokens, t.cache_read_tokens, t.cache_creation_tokens, t.reasoning_tokens
    ));
    if let Some(cost) = t.cost_usd {
        out.push_str(&format!("- Cost: ${cost:.4}\n"));
    }
    out.push_str(&format!("- Detail: {}\n", record.detail.as_str()));
    if let Some(source) = source.filter(|s| !s.is_empty()) {
        out.push_str(&format!("- Source: {source} (agent_log)\n"));
        out.push_str(&format!(
            "- Transcript: [open in Agent sessions](wiki.html?view=sessions&source={source})\n"
        ));
    }
    if record.truncated {
        out.push_str(&format!(
            "- Truncated: {} turn(s) removed to fit the upload size cap\n",
            record.omitted_turns
        ));
    }
    out
}

fn turn_block(turn: &Turn, limits: Limits) -> String {
    let at = turn.at.as_deref().map(short_time).unwrap_or_default();
    let stamp = if at.is_empty() { String::new() } else { format!(" · {at}") };
    let mut out = String::new();
    match turn.role {
        Role::User => {
            if turn.text.trim().is_empty() {
                return String::new();
            }
            out.push_str(&format!("**User**{stamp}\n\n"));
            out.push_str(&message(&turn.text, limits.text));
        }
        Role::Assistant => {
            if !turn.text.trim().is_empty() {
                out.push_str(&format!("**Assistant**{stamp}\n\n"));
                out.push_str(&message(&turn.text, limits.text));
            }
            for call in &turn.tool_calls {
                out.push_str(&call_block(call, limits));
            }
        }
        Role::Reasoning => {
            let text = clip(turn.text.trim(), limits.text);
            out.push_str(&format!("_Reasoning{stamp}_\n\n"));
            for line in text.lines() {
                out.push_str(&format!("> {line}\n"));
            }
            out.push('\n');
        }
        Role::Event => out.push_str(&format!("_— {}{stamp} —_\n\n", one_line(&turn.text))),
    }
    out
}

fn message(text: &str, max: usize) -> String {
    let mut text = clip(text.trim(), max);
    if text.lines().filter(|line| line.trim_start().starts_with("```")).count() % 2 == 1 {
        text.push_str("\n```");
    }
    format!("{text}\n\n")
}

fn call_block(call: &ToolCall, limits: Limits) -> String {
    let mut line = format!("- `{}`", call.name.replace('`', "'"));
    if let Some(path) = &call.path {
        line.push_str(&format!(" `{}`", one_line(path).replace('`', "'")));
    }
    if let Some(status) = call.status {
        line.push_str(&format!(" · {status}"));
    }
    if let Some(code) = call.exit_code {
        line.push_str(&format!(" · exit {code}"));
    }
    let mut out = format!("{line}\n\n");
    if let Some(input) = &call.input {
        let lang = match call.format {
            Some("shell") => "sh",
            Some(other) => other,
            None => "text",
        };
        out.push_str(&fenced(lang, &clip(input, limits.input)));
    }
    if let Some(output) = call.output.as_ref().filter(|_| limits.output > 0) {
        out.push_str(&fenced("text", &clip(output, limits.output)));
    }
    out
}

fn fenced(lang: &str, body: &str) -> String {
    let mut out = format!("```{lang}\n");
    for line in body.trim_end_matches('\n').lines() {
        if line.trim_start().starts_with("```") {
            out.push('\u{200b}');
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("```\n\n");
    out
}

fn short_time(at: &str) -> String {
    let at = at.trim_end_matches('Z');
    let at = at.split('.').next().unwrap_or(at);
    at.replacen('T', " ", 1)
}

fn escaped_len(text: &str) -> usize {
    serde_json::to_string(text).map_or(text.len(), |escaped| escaped.len())
}

fn tool_summary(turns: &[Turn]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for call in turns.iter().flat_map(|turn| &turn.tool_calls) {
        *counts.entry(call.name.as_str()).or_default() += 1;
    }
    let total: usize = counts.values().sum();
    if total == 0 {
        return "0".to_owned();
    }
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let names: Vec<String> = ranked
        .iter()
        .map(|(name, count)| format!("{} ×{count}", one_line(name)))
        .collect();
    format!("{total} ({})", names.join(", "))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
