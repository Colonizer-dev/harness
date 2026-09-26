//! One-line task summaries: a cheap model reads a colony's issue (or its instructions) and writes
//! the task as one plain sentence, so the cockpit can say what a colony is doing where it otherwise
//! shows "open session" or a long issue title. Written once after launch, again from the pull
//! request when it opens, and backfilled at startup for colonies that have none — live ones first, then
//! the most recently updated ended ones, so the Overview's history reads as summaries too.
//!
//! Only the task text leaves the host — the issue title and body, the instructions, or the pull
//! request's title and body — never a secret, a path on the host, or colony output. A failure
//! leaves `summary` unset: the cockpit falls back to the title, and nothing waits on this.
//!
//! The model is reached like the autonomy judge's: through a model provider the operator configured
//! (the gateway's provider config and saved key), or with a real Anthropic API key. Never with the
//! Claude subscription token — that is issued for Claude Code inside colonies, not for the
//! mothership's own calls — so an install with only that token writes no summaries.

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
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Bounded `gh` reads of the issue or pull request body.
const GH_TIMEOUT: Duration = Duration::from_secs(15);
/// How many colonies one start backfills (live first, then the most recent ended ones), and how
/// many at once.
const BACKFILL_CAP: usize = 200;
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

/// How a summary request is sent.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    /// Through a configured model provider, as the autonomy judge does: `<provider>/<model>`, or a
    /// plain Claude model on the operator's Anthropic provider.
    Provider(String),
    /// Straight to the Anthropic API with a real API key (`sk-ant-api…`) as `x-api-key`.
    ApiKey(String),
}

/// The Anthropic API key in a Claude credential, or `None` — a `claude setup-token` subscription
/// token is never one, so it is never used for summaries. Pure, for the tests.
pub fn api_key_of(credential: &str) -> Option<&str> {
    let c = credential.trim();
    c.starts_with("sk-ant-api").then_some(c)
}

/// Which model writes summaries, in order: the `summary_model` setting; else the first of
/// `candidates` (the agent's subagent, low-tier and background models) that names a configured
/// `<provider>/<model>`; else Claude Haiku with a real API key; else none. A plain Claude model in
/// `summary_model` goes through the operator's Anthropic provider when there is one, else the API
/// key. Pure, for the tests.
pub fn choose(
    summary_model: &str,
    candidates: &[&str],
    provider_ids: &[&str],
    has_anthropic_provider: bool,
    has_api_key: bool,
) -> Option<Route> {
    let configured = |m: &str| {
        m.split_once('/')
            .is_some_and(|(id, rest)| !rest.is_empty() && provider_ids.contains(&id))
    };
    let chosen = summary_model.trim();
    if !chosen.is_empty() {
        if chosen.contains('/') {
            if configured(chosen) {
                return Some(Route::Provider(chosen.to_string()));
            }
        } else if has_anthropic_provider {
            return Some(Route::Provider(chosen.to_string()));
        } else if has_api_key {
            return Some(Route::ApiKey(chosen.to_string()));
        }
    }
    if let Some(m) = candidates.iter().map(|m| m.trim()).find(|m| configured(m)) {
        return Some(Route::Provider(m.to_string()));
    }
    has_api_key.then(|| Route::ApiKey(FALLBACK_MODEL.to_string()))
}

/// Whether summaries are on (the agent module's `summaries` setting, default on) and how they are
/// written; `None` when no model is available.
async fn settings(app: &Shared) -> (bool, Option<Route>) {
    if std::env::var("COLONIZER_SUMMARIES").is_ok_and(|v| matches!(v.trim(), "0" | "false" | "off")) {
        return (false, None);
    }
    let modules = app.modules.read().await.clone();
    let (on, summary_model, candidates) = match modules.get("agent") {
        Some(choice) => {
            let schema = modules::schema_for("agent", &choice.provider, &app.agents);
            let on = setting(choice, &schema, "summaries").and_then(Value::as_bool).unwrap_or(true);
            let candidates = ["subagent_model", "model_low", "background_model"].map(|k| setting_str(choice, &schema, k));
            (on, setting_str(choice, &schema, "summary_model"), candidates.to_vec())
        }
        None => (true, String::new(), Vec::new()),
    };
    if !on {
        return (false, None);
    }
    let providers = app.providers();
    let ids: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
    let has_anthropic = providers.iter().any(|p| {
        crate::providers::split_url(&p.base_url).is_some_and(|(_, host, _, _)| host.eq_ignore_ascii_case(crate::CLAUDE_API_HOST))
    });
    let has_api_key = app.claude_cred().is_some_and(|c| api_key_of(&c.value).is_some());
    let candidates: Vec<&str> = candidates.iter().map(String::as_str).collect();
    (true, choose(&summary_model, &candidates, &ids, has_anthropic, has_api_key))
}

/// The cheap model the install would write summaries with, whether or not summaries are switched
/// on: the default for anything else that wants a quick, inexpensive answer (the cockpit's chat).
pub async fn cheap_route(app: &Shared) -> Option<Route> {
    let modules = app.modules.read().await.clone();
    let (summary_model, candidates) = match modules.get("agent") {
        Some(choice) => {
            let schema = modules::schema_for("agent", &choice.provider, &app.agents);
            let candidates = ["subagent_model", "model_low", "background_model"].map(|k| setting_str(choice, &schema, k));
            (setting_str(choice, &schema, "summary_model"), candidates.to_vec())
        }
        None => (String::new(), Vec::new()),
    };
    let providers = app.providers();
    let ids: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
    let has_anthropic = providers.iter().any(|p| {
        crate::providers::split_url(&p.base_url).is_some_and(|(_, host, _, _)| host.eq_ignore_ascii_case(crate::CLAUDE_API_HOST))
    });
    let has_api_key = app.claude_cred().is_some_and(|c| api_key_of(&c.value).is_some());
    let candidates: Vec<&str> = candidates.iter().map(String::as_str).collect();
    choose(&summary_model, &candidates, &ids, has_anthropic, has_api_key)
}

/// Asks for a summary of `input` along `route`, bounded by [`REQUEST_TIMEOUT`].
async fn ask(app: &Shared, route: &Route, input: &str) -> Result<String, String> {
    match route {
        Route::Provider(model) => {
            let prompt = format!("{SYSTEM_PROMPT}\n\nThe task:\n{input}");
            match tokio::time::timeout(REQUEST_TIMEOUT, crate::autonomy::ask_model(app, model, &prompt)).await {
                Ok(Ok(text)) => Ok(text),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(_) => Err(format!("{model} did not answer within {}s", REQUEST_TIMEOUT.as_secs())),
            }
        }
        Route::ApiKey(model) => {
            let key = app
                .claude_cred()
                .and_then(|c| api_key_of(&c.value).map(str::to_string))
                .ok_or("no Anthropic API key")?;
            ask_with_api_key(&key, model, input).await
        }
    }
}

/// One request straight to the Anthropic API with a real API key.
async fn ask_with_api_key(key: &str, model: &str, input: &str) -> Result<String, String> {
    let body = json!({
        "model": crate::providers::api_model(model),
        "max_tokens": 60,
        "temperature": 0,
        "system": SYSTEM_PROMPT,
        "messages": [{"role": "user", "content": input}],
    });
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("could not build an HTTP client: {e}"))?;
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    let status = response.status();
    let answer: Value = response.json().await.map_err(|e| format!("unreadable answer: {e}"))?;
    if !status.is_success() {
        let message = answer["error"]["message"].as_str().unwrap_or("no message");
        return Err(format!("Anthropic answered {status}: {message}"));
    }
    Ok(answer["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The model route the summaries use, whether or not summaries are switched on: for other
/// quick answers (the Code page's "Answer here"). Never the Claude subscription token.
async fn route_only(app: &Shared) -> Option<Route> {
    let modules = app.modules.read().await.clone();
    let (summary_model, candidates) = match modules.get("agent") {
        Some(choice) => {
            let schema = modules::schema_for("agent", &choice.provider, &app.agents);
            let candidates = ["subagent_model", "model_low", "background_model"].map(|k| setting_str(choice, &schema, k));
            (setting_str(choice, &schema, "summary_model"), candidates.to_vec())
        }
        None => (String::new(), Vec::new()),
    };
    let providers = app.providers();
    let ids: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
    let has_anthropic = providers.iter().any(|p| {
        crate::providers::split_url(&p.base_url).is_some_and(|(_, host, _, _)| host.eq_ignore_ascii_case(crate::CLAUDE_API_HOST))
    });
    let has_api_key = app.claude_cred().is_some_and(|c| api_key_of(&c.value).is_some());
    let candidates: Vec<&str> = candidates.iter().map(String::as_str).collect();
    choose(&summary_model, &candidates, &ids, has_anthropic, has_api_key)
}

/// A free-form answer to `prompt` from the cheap summary model, with the model it came from. The
/// prompt carries its own instructions; bounded to a minute.
pub async fn ask_freeform(app: &Shared, prompt: &str) -> Result<(String, String), String> {
    let route = route_only(app)
        .await
        .ok_or("no model for quick answers: set summary_model or a provider key in Settings → Agent")?;
    let model = match &route {
        Route::Provider(m) | Route::ApiKey(m) => m.clone(),
    };
    let answer = match &route {
        Route::Provider(m) => {
            match tokio::time::timeout(Duration::from_secs(60), crate::autonomy::ask_model(app, m, prompt)).await {
                Ok(Ok(text)) => text,
                Ok(Err(e)) => return Err(format!("{e:#}")),
                Err(_) => return Err(format!("{m} did not answer within 60s")),
            }
        }
        Route::ApiKey(m) => {
            let key = app
                .claude_cred()
                .and_then(|c| api_key_of(&c.value).map(str::to_string))
                .ok_or("no Anthropic API key")?;
            ask_freeform_with_api_key(&key, m, prompt).await?
        }
    };
    Ok((answer, model))
}

async fn ask_freeform_with_api_key(key: &str, model: &str, prompt: &str) -> Result<String, String> {
    let body = json!({
        "model": crate::providers::api_model(model),
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": prompt}],
    });
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("could not build an HTTP client: {e}"))?;
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    let status = response.status();
    let answer: Value = response.json().await.map_err(|e| format!("unreadable answer: {e}"))?;
    if !status.is_success() {
        let message = answer["error"]["message"].as_str().unwrap_or("no message");
        return Err(format!("Anthropic answered {status}: {message}"));
    }
    Ok(answer["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Logs a summary failure once per run.
fn log_once(message: String) {
    if !FAILURE_LOGGED.swap(true, Ordering::SeqCst) {
        eprintln!("summaries: {message}");
    }
}

/// Writes (or rewrites) a colony's summary from `title` and `body`. Quiet on failure after the first.
async fn write(app: &Shared, id: &str, title: &str, body: &str) {
    let (on, route) = settings(app).await;
    if !on {
        return;
    }
    let Some(route) = route else {
        log_once("no model for summaries: set summary_model or a provider key".into());
        return;
    };
    let input = build_input(title, body);
    if input.is_empty() {
        return;
    }
    match ask(app, &route, &input).await.map(|text| clean(&text)) {
        Ok(Some(summary)) => {
            app.update_session(id, |x| x.summary = Some(summary)).await;
        }
        Ok(None) => {}
        Err(reason) => log_once(format!(
            "could not summarize colony {id} ({reason}); colonies show their titles instead"
        )),
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
    if !matches!(settings(&app).await, (true, Some(_))) {
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
    if !matches!(settings(&app).await, (true, Some(_))) {
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

/// At startup: summarizes up to [`BACKFILL_CAP`] colonies that have none — live ones first, then the
/// most recently updated ended ones — [`BACKFILL_PARALLEL`] at a time.
pub async fn backfill(app: Shared) {
    if !matches!(settings(&app).await, (true, Some(_))) {
        return;
    }
    let mut missing: Vec<(bool, chrono::DateTime<chrono::Utc>, String)> = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.summary.is_none())
        .map(|s| (live(s.status), s.updated_at, s.id.clone()))
        .collect();
    let ids = backfill_order(&mut missing);
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

/// The backfill order: live colonies first, then the rest, each newest first; capped.
fn backfill_order(missing: &mut [(bool, chrono::DateTime<chrono::Utc>, String)]) -> Vec<String> {
    missing.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    missing.iter().take(BACKFILL_CAP).map(|(_, _, id)| id.clone()).collect()
}

/// One-line task summaries for live colonies that have none.
pub(crate) fn start_tasks(app: &crate::Shared) {
    tokio::spawn(backfill(app.clone()));
}

#[cfg(test)]
mod backfill_order_tests {
    use super::*;

    #[test]
    fn live_first_then_the_newest_ended_ones() {
        let t = |h: i64| chrono::Utc::now() - chrono::Duration::hours(h);
        let mut m = vec![
            (false, t(1), "ended-new".into()),
            (true, t(5), "live-old".into()),
            (false, t(9), "ended-old".into()),
            (true, t(2), "live-new".into()),
        ];
        assert_eq!(backfill_order(&mut m), vec!["live-new", "live-old", "ended-new", "ended-old"]);
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
    fn the_summary_model_wins_then_a_routed_provider_model_then_an_api_key() {
        let ids = ["zai", "bailian"];
        // 1. summary_model, when it names a configured provider.
        assert_eq!(
            choose("bailian/qwen-flash", &["zai/glm-5.3-flash"], &ids, false, true),
            Some(Route::Provider("bailian/qwen-flash".into()))
        );
        // A plain Claude model goes through the Anthropic provider, else the API key.
        assert_eq!(
            choose("claude-haiku-4-5", &[], &ids, true, false),
            Some(Route::Provider("claude-haiku-4-5".into()))
        );
        assert_eq!(
            choose("claude-haiku-4-5", &[], &ids, false, true),
            Some(Route::ApiKey("claude-haiku-4-5".into()))
        );
        // 2. The first routed <provider>/<model> the agent already uses.
        assert_eq!(
            choose("", &["claude-sonnet-5", "zai/glm-5.3-flash", "bailian/x"], &ids, false, true),
            Some(Route::Provider("zai/glm-5.3-flash".into()))
        );
        // An unconfigured provider is skipped.
        assert_eq!(choose("nosuch/m", &["nosuch/m"], &ids, false, false), None);
        // 3. A real API key with Haiku.
        assert_eq!(
            choose("", &["claude-sonnet-5"], &ids, false, true),
            Some(Route::ApiKey(FALLBACK_MODEL.into()))
        );
        // 4. Nothing.
        assert_eq!(choose("", &["claude-sonnet-5"], &ids, false, false), None);
    }

    #[test]
    fn a_subscription_token_is_never_an_api_key() {
        assert_eq!(api_key_of("sk-ant-api03-abc"), Some("sk-ant-api03-abc"));
        assert_eq!(
            api_key_of("sk-ant-oat01-abc"),
            None,
            "a claude setup-token OAuth token is never used"
        );
        assert_eq!(api_key_of(""), None);
        let ids = ["zai"];
        let has_api_key = api_key_of("sk-ant-oat01-abc").is_some();
        assert_eq!(choose("", &["claude-sonnet-5"], &ids, false, has_api_key), None);
    }
}
