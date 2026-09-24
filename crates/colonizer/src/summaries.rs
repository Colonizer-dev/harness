//! One-line task summaries: a cheap model reads a colony's issue (or its instructions) and writes
//! the task as one plain sentence, so the cockpit can say what a colony is doing where it otherwise
//! shows "open session" or a long issue title. Written once after launch, again from the pull
//! request when it opens, and backfilled at startup for live colonies that have none.
//!
//! Only the task text leaves the host — the issue title and body, the instructions, or the pull
//! request's title and body — never a secret, a path on the host, or colony output. A failure
//! leaves `summary` unset: the cockpit falls back to the title, and nothing waits on this.

use crate::{
    Shared,
    config::{setting, setting_str},
    modules,
    sessions::SessionStatus,
    util::exec_within,
};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// The cheapest Claude model, used when the agent module's background model is not a Claude one.
pub const FALLBACK_MODEL: &str = "claude-haiku-4-5";
/// The instruction the summary model gets, verbatim.
pub const SYSTEM_PROMPT: &str =
    "Summarize the coding task in one plain sentence of at most 15 words, imperative mood, no quotes, no trailing period.";
/// How much task text is sent: the head of the issue is where the task is stated.
const INPUT_LIMIT: usize = 6 * 1024;
/// The longest summary kept.
pub const SUMMARY_LIMIT: usize = 120;
/// A summary request never outlasts this; the cockpit falls back to the title meanwhile.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounded `gh` reads of the issue or pull request body.
const GH_TIMEOUT: Duration = Duration::from_secs(15);
/// How many live colonies one start backfills, and how many at once.
const BACKFILL_CAP: usize = 50;
const BACKFILL_PARALLEL: usize = 2;

/// Set once the first failure is logged, so a missing credential or an unreachable API does not
/// print a line per colony.
static FAILURE_LOGGED: AtomicBool = AtomicBool::new(false);

/// The text the model is given: the title, then the body, cut to [`INPUT_LIMIT`] bytes on a
/// character boundary. Pure, for the tests.
pub fn build_input(title: &str, body: &str) -> String {
    let title = title.trim();
    let body = body.trim();
    let text = match (title.is_empty(), body.is_empty()) {
        (false, false) => format!("{title}\n\n{body}"),
        (false, true) => title.to_string(),
        (true, false) => body.to_string(),
        (true, true) => String::new(),
    };
    if text.len() <= INPUT_LIMIT {
        return text;
    }
    let mut end = INPUT_LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// The model's answer as a summary: its first non-empty line, whitespace collapsed, wrapping quotes
/// and a trailing period stripped, clamped to [`SUMMARY_LIMIT`] characters with an ellipsis. `None`
/// when nothing is left. Pure, for the tests.
pub fn clean(answer: &str) -> Option<String> {
    let line = answer.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut text: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let quotes: &[char] = &['"', '\'', '“', '”', '‘', '’', '`'];
    text = text.trim_matches(quotes).trim().to_string();
    for prefix in ["Summary:", "summary:", "Task:", "task:"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim().trim_matches(quotes).trim().to_string();
        }
    }
    while text.ends_with('.') {
        text.pop();
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= SUMMARY_LIMIT {
        return Some(text);
    }
    let cut: String = text.chars().take(SUMMARY_LIMIT - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// The model to ask: the agent module's background model when it names a Claude model (an alias or
/// ID, no `<provider>/` prefix), else [`FALLBACK_MODEL`]. Pure, for the tests.
pub fn model_for(background_model: &str) -> String {
    let m = background_model.trim();
    if m.is_empty() || m.contains('/') {
        FALLBACK_MODEL.to_string()
    } else {
        m.to_string()
    }
}

/// Whether summaries are on (the agent module's `summaries` setting, default on) and which model
/// writes them.
async fn settings(app: &Shared) -> (bool, String) {
    if std::env::var("COLONIZER_SUMMARIES").is_ok_and(|v| matches!(v.trim(), "0" | "false" | "off")) {
        return (false, String::new());
    }
    let modules = app.modules.read().await.clone();
    let Some(choice) = modules.get("agent") else {
        return (true, FALLBACK_MODEL.to_string());
    };
    let schema = modules::schema_for("agent", &choice.provider, &app.agents);
    let on = setting(choice, &schema, "summaries").and_then(Value::as_bool).unwrap_or(true);
    (on, model_for(&setting_str(choice, &schema, "background_model")))
}

/// Asks the model for a summary of `input`, with the mothership's own Claude credential.
async fn ask(app: &Shared, model: &str, input: &str) -> Result<String, String> {
    let cred = app.claude_cred().ok_or("no Claude credential is configured")?;
    let oauth = cred.env != "ANTHROPIC_API_KEY";
    // A subscription (OAuth) token is only accepted for Claude Code's own requests, which open with
    // its identity line; an API key takes the instruction alone.
    let system = if oauth {
        json!([
            {"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."},
            {"type": "text", "text": SYSTEM_PROMPT},
        ])
    } else {
        json!(SYSTEM_PROMPT)
    };
    let body = json!({
        "model": model,
        "max_tokens": 60,
        "temperature": 0,
        "system": system,
        "messages": [{"role": "user", "content": input}],
    });
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("could not build an HTTP client: {e}"))?;
    let mut request = client
        .post("https://api.anthropic.com/v1/messages")
        .header("anthropic-version", "2023-06-01")
        .json(&body);
    request = if oauth {
        request.bearer_auth(&cred.value).header("anthropic-beta", "oauth-2025-04-20")
    } else {
        request.header("x-api-key", &cred.value)
    };
    let response = request.send().await.map_err(|e| format!("{e}"))?;
    let status = response.status();
    let answer: Value = response.json().await.map_err(|e| format!("unreadable answer: {e}"))?;
    if !status.is_success() {
        let message = answer["error"]["message"].as_str().unwrap_or("no message");
        return Err(format!("Anthropic answered {status}: {message}"));
    }
    let text = answer["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
}

/// Writes (or rewrites) a colony's summary from `title` and `body`. Quiet on failure after the first.
async fn write(app: &Shared, id: &str, title: &str, body: &str) {
    let (on, model) = settings(app).await;
    if !on {
        return;
    }
    let input = build_input(title, body);
    if input.is_empty() {
        return;
    }
    match ask(app, &model, &input).await.map(|text| clean(&text)) {
        Ok(Some(summary)) => {
            app.update_session(id, |x| x.summary = Some(summary)).await;
        }
        Ok(None) => {}
        Err(reason) => {
            if !FAILURE_LOGGED.swap(true, Ordering::SeqCst) {
                eprintln!("summaries: could not summarize colony {id} ({reason}); colonies show their titles instead");
            }
        }
    }
}

/// The issue's body through `gh`, bounded; empty when it cannot be read.
async fn issue_body(app: &Shared, repo: &str, issue: u64) -> String {
    let number = issue.to_string();
    let mut cmd = app.gh([
        "issue",
        "view",
        number.as_str(),
        "-R",
        repo,
        "--json",
        "body",
        "--jq",
        ".body",
    ]);
    exec_within(GH_TIMEOUT, &mut cmd).await.unwrap_or_default()
}

/// Summarizes a colony from its issue (title and body) or, for an open session, its instructions.
pub async fn summarize_colony(app: Shared, id: String) {
    // Nothing is fetched (not even the issue) when summaries are off or no model can be asked.
    if !settings(&app).await.0 || app.claude_cred().is_none() {
        return;
    }
    let Some(s) = app.session(&id).await else { return };
    let body = match s.issue {
        Some(issue) => {
            let body = issue_body(&app, &s.repo, issue).await;
            if s.instructions.trim().is_empty() {
                body
            } else {
                format!("{body}\n\n{}", s.instructions)
            }
        }
        None => s.instructions.clone(),
    };
    write(&app, &id, &s.issue_title, &body).await;
}

/// Rewrites a colony's summary from its pull request, which says what was actually done.
pub async fn summarize_pull_request(app: Shared, id: String, url: String) {
    if !settings(&app).await.0 || app.claude_cred().is_none() {
        return;
    }
    let mut cmd = app.gh(["pr", "view", url.as_str(), "--json", "title,body"]);
    let Ok(out) = exec_within(GH_TIMEOUT, &mut cmd).await else {
        return;
    };
    let Ok(pr) = serde_json::from_str::<Value>(&out) else { return };
    let title = pr["title"].as_str().unwrap_or_default().to_string();
    let body = pr["body"].as_str().unwrap_or_default().to_string();
    write(&app, &id, &title, &body).await;
}

/// Whether a status is one the cockpit shows as a live colony, worth a summary at startup.
fn live(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Queued
            | SessionStatus::Starting
            | SessionStatus::Running
            | SessionStatus::WaitingForAnswer
            | SessionStatus::Idle
            | SessionStatus::Publishing
            | SessionStatus::PrOpened
    )
}

/// At startup: summarizes up to [`BACKFILL_CAP`] live colonies that have none, [`BACKFILL_PARALLEL`]
/// at a time.
pub async fn backfill(app: Shared) {
    if !settings(&app).await.0 {
        return;
    }
    let ids: Vec<String> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.summary.is_none() && live(s.status))
        .map(|s| s.id.clone())
        .take(BACKFILL_CAP)
        .collect();
    let limit = std::sync::Arc::new(tokio::sync::Semaphore::new(BACKFILL_PARALLEL));
    let mut tasks = Vec::new();
    for id in ids {
        let Ok(permit) = limit.clone().acquire_owned().await else {
            break;
        };
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            summarize_colony(app, id).await;
            drop(permit);
        }));
    }
    for task in tasks {
        let _ = task.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_is_title_then_body_cut_on_a_char_boundary() {
        assert_eq!(
            build_input(" Fix login ", "  Users are logged out.  "),
            "Fix login\n\nUsers are logged out."
        );
        assert_eq!(build_input("Only a title", ""), "Only a title");
        assert_eq!(build_input("", ""), "");
        let long = format!("{}é", "a".repeat(INPUT_LIMIT - 1));
        let cut = build_input("", &long);
        assert!(cut.len() <= INPUT_LIMIT);
        assert!(
            cut.chars().all(|c| c == 'a'),
            "a split multi-byte char is dropped, not broken"
        );
    }

    #[test]
    fn answers_are_cleaned_to_one_plain_sentence() {
        assert_eq!(
            clean("\"Fix the login redirect loop.\"").as_deref(),
            Some("Fix the login redirect loop")
        );
        assert_eq!(
            clean("\n  Summary: Add retries to the gateway...\nmore").as_deref(),
            Some("Add retries to the gateway")
        );
        assert_eq!(clean("   "), None);
        let long = "word ".repeat(60);
        let out = clean(&long).unwrap();
        assert!(out.chars().count() <= SUMMARY_LIMIT);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn a_claude_background_model_is_used_and_anything_else_falls_back() {
        assert_eq!(model_for("claude-haiku-4-5"), "claude-haiku-4-5");
        assert_eq!(model_for("sonnet"), "sonnet");
        assert_eq!(model_for("bailian/deepseek-v4-flash"), FALLBACK_MODEL);
        assert_eq!(model_for(""), FALLBACK_MODEL);
    }
}
