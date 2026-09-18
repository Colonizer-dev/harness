//! Autonomous mode: a judge model answers a colony's questions when nobody does.
//!
//! A colony that asks a question stops until a person answers. That is right when someone is
//! watching, and it is the whole cost of the thing when nobody is. With this on, a question left
//! unanswered goes to a judge — a model chosen per install, which can be a better one than the
//! colony itself runs — and the colony carries on.
//!
//! The judge chooses **among the options the agent offered**, and nothing else. That is the line
//! that keeps this bounded: a colony reads repository content, content can carry instructions, and a
//! judge reading the same content can be steered by it. So a reply that is not one of the offered
//! labels is not an answer — it escalates to the person instead of guessing — free text is refused
//! unless it is switched on, and a colony that keeps asking is flagged rather than driven.
//!
//! Every judged answer is recorded as judged: in the session log with the model and the reason, and
//! in the answer the colony receives, so a pull request that came out of autonomous mode reads as
//! one afterwards.

use crate::{
    App, Shared,
    config::{ModulesConfig, setting_str, setting_u64},
    modules::schema_for,
    providers::{Provider, api_model},
    sessions::SessionStatus,
    util::truncate,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::time::Duration;

/// How long a judged reply may be: a label and a sentence, not an essay.
const MAX_TOKENS: u64 = 1_024;
/// Question text and option labels a colony sends are agent output; bound them before they become a
/// prompt on this side.
const MAX_FIELD: usize = 2_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Judge {
    pub model: String,
    /// Minutes a question waits for a person first. Zero answers as soon as it is seen.
    pub after_minutes: u64,
    pub max_answers: u64,
    pub free_text: bool,
}

/// The judge this install is configured with, or `None` when autonomous mode is off or has no model.
pub fn judge(modules: &ModulesConfig, agents: &[crate::modules::AgentModule]) -> Option<Judge> {
    let choice = modules.autonomy.as_ref()?;
    if !choice.enabled || choice.provider != "judge" {
        return None;
    }
    let schema = schema_for("autonomy", "judge", agents);
    let model = setting_str(choice, &schema, "model").trim().to_string();
    if model.is_empty() {
        return None;
    }
    Some(Judge {
        model,
        after_minutes: setting_u64(choice, &schema, "after_minutes"),
        max_answers: setting_u64(choice, &schema, "max_answers"),
        free_text: choice.settings.get("free_text").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Why a question was not answered by the judge. Each one ends with the person, not a guess.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    /// The model's reply was not the JSON asked for.
    Unparsable,
    /// A chosen label is not one the agent offered — the case this check exists for.
    NotOffered(String),
    /// A question has no options and free text is off.
    FreeTextRefused(String),
    /// The reply left a question out.
    Incomplete(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unparsable => write!(f, "the judge's reply was not the JSON it was asked for"),
            Self::NotOffered(label) => write!(f, "the judge chose {label:?}, which is not one of the offered options"),
            Self::FreeTextRefused(q) => write!(f, "{q:?} wants free text, and free-text answers are switched off"),
            Self::Incomplete(q) => write!(f, "the judge left {q:?} unanswered"),
        }
    }
}

fn field(value: &Value) -> String {
    truncate(value.as_str().unwrap_or_default().trim(), MAX_FIELD)
}

/// The labels one question offers, in order.
fn options(question: &Value) -> Vec<String> {
    question["options"]
        .as_array()
        .map(|o| o.iter().map(|opt| field(&opt["label"])).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// What the judge is asked. The task is what the colony is for; the questions are as the agent put
/// them, options included, because choosing among them is the whole job.
pub fn prompt(task: &str, questions: &[Value]) -> String {
    let mut out = String::new();
    out.push_str("A coding agent working on the task below has stopped to ask. Answer for the person who is not here.\n\n");
    out.push_str("## The task\n\n");
    out.push_str(truncate(task.trim(), 6_000).trim());
    out.push_str("\n\n## The questions\n\n");
    for question in questions {
        out.push_str(&format!("### {}\n", field(&question["question"])));
        if question["multi_select"].as_bool().unwrap_or(false) {
            out.push_str("(more than one option may be chosen)\n");
        }
        for option in question["options"].as_array().unwrap_or(&Vec::new()) {
            let description = field(&option["description"]);
            let label = field(&option["label"]);
            if description.is_empty() {
                out.push_str(&format!("- {label}\n"));
            } else {
                out.push_str(&format!("- {label} — {description}\n"));
            }
        }
        out.push('\n');
    }
    out.push_str(
        "Reply with JSON only, no prose around it:\n\n\
         {\"answers\": {\"<the question, exactly as written above>\": \"<the option label, exactly as written above>\"}, \
         \"reason\": \"<one sentence>\"}\n\n\
         Use an array of labels for a question that allows more than one. Copy labels exactly; a label that is not \
         offered is not an answer. Choose what serves the task, and prefer the option that is easiest to undo when it \
         is close.",
    );
    out
}

/// Reads the judge's reply and checks it against what was actually offered.
pub fn decide(reply: &str, questions: &[Value], free_text: bool) -> Result<(Map<String, Value>, String), Refusal> {
    let start = reply.find('{').ok_or(Refusal::Unparsable)?;
    let end = reply.rfind('}').ok_or(Refusal::Unparsable)?;
    let parsed: Value = serde_json::from_str(reply.get(start..=end).unwrap_or_default()).map_err(|_| Refusal::Unparsable)?;
    let given = parsed["answers"].as_object().ok_or(Refusal::Unparsable)?;
    let reason = truncate(parsed["reason"].as_str().unwrap_or_default().trim(), 500);

    let mut answers = Map::new();
    for question in questions {
        let text = field(&question["question"]);
        let offered = options(question);
        let answer = given.get(&text).ok_or_else(|| Refusal::Incomplete(text.clone()))?;
        if offered.is_empty() {
            // No options: the agent wants free text, which is the answer that can say anything.
            if !free_text {
                return Err(Refusal::FreeTextRefused(text));
            }
            let Some(written) = answer.as_str() else {
                return Err(Refusal::Unparsable);
            };
            answers.insert(text, json!(truncate(written.trim(), MAX_FIELD)));
            continue;
        }
        let chosen: Vec<String> = match answer {
            Value::String(one) => vec![one.trim().to_string()],
            Value::Array(many) => many.iter().filter_map(|l| l.as_str().map(|l| l.trim().to_string())).collect(),
            _ => return Err(Refusal::Unparsable),
        };
        if chosen.is_empty() {
            return Err(Refusal::Incomplete(text));
        }
        for label in &chosen {
            if !offered.iter().any(|o| o == label) {
                return Err(Refusal::NotOffered(label.clone()));
            }
        }
        let multi = question["multi_select"].as_bool().unwrap_or(false);
        answers.insert(text, if multi { json!(chosen) } else { json!(chosen[0]) });
    }
    Ok((answers, reason))
}

/// Where a judged request goes, and what proves this install may make it.
///
/// A `provider/model` id goes to that provider with the API key the operator saved for it. A plain
/// id goes to Anthropic with the saved Claude credential — which, when it is a subscription token
/// from `claude setup-token`, was issued for Claude Code to use inside a colony. Answering questions
/// from the Mothership is a different use of it, switched on deliberately (issue #143).
async fn ask_model(app: &App, model: &str, prompt: &str) -> Result<String> {
    let body = json!({
        "model": api_model(model.rsplit('/').next().unwrap_or(model)),
        "max_tokens": MAX_TOKENS,
        "messages": [{"role": "user", "content": prompt}],
    });
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?;
    let request = match model.split_once('/') {
        Some((id, _)) => {
            let provider: Provider = app
                .providers()
                .into_iter()
                .find(|p| p.id == id)
                .with_context(|| format!("no model provider called {id:?}"))?;
            let mut request = client.post(format!("{}/v1/messages", provider.base_url.trim_end_matches('/')));
            if let Some((name, value)) = crate::gateway::credential_header(app, &provider) {
                request = request.header(name, value);
            }
            request.header("anthropic-version", "2023-06-01")
        }
        None => {
            let cred = app
                .claude_cred()
                .context("no Claude credential; log in in Settings or pick a provider/model")?;
            let request = client
                .post(format!("https://{}/v1/messages", crate::CLAUDE_API_HOST))
                .header("anthropic-version", "2023-06-01");
            if cred.env == "ANTHROPIC_API_KEY" {
                request.header("x-api-key", cred.value)
            } else {
                request
                    .header("authorization", format!("Bearer {}", cred.value))
                    .header("anthropic-beta", "oauth-2025-04-20")
            }
        }
    };
    let response = request.json(&body).send().await.context("the judge's model is unreachable")?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{model} answered {status}: {}", truncate(&text, 300));
    }
    let value: Value = serde_json::from_str(&text).context("the model's reply was not JSON")?;
    value["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find_map(|b| b["text"].as_str()))
        .map(str::to_string)
        .context("the model's reply carried no text")
}

/// Answers one colony's open question. Returns the line to log, or the reason it was left alone.
async fn judge_one(app: &Shared, id: &str, judge: &Judge, task: &str, question_id: &str, questions: &[Value]) -> Result<String> {
    let reply = ask_model(app, &judge.model, &prompt(task, questions)).await?;
    let (answers, reason) = decide(&reply, questions, judge.free_text).map_err(|refusal| anyhow::anyhow!("{refusal}"))?;
    let Some(rt) = app.runtimes.lock().await.get(id).cloned() else {
        bail!("the colony is gone")
    };
    let chosen = answers.values().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
    rt.send_command(json!({
        "type": "answer",
        "question_id": question_id,
        "answers": answers,
        // The colony is told, so the agent knows it is running unattended.
        "response": format!("Answered automatically by {} in autonomous mode, with nobody watching: {reason}", judge.model),
    }));
    rt.activity.lock().await.judged += 1;
    Ok(format!("autonomous: {} answered with {chosen} — {reason}", judge.model))
}

/// Every half minute, looks for a question nobody has answered.
pub async fn run(app: Shared) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let modules = app.modules.read().await.clone();
        let Some(judge) = judge(&modules, &app.agents) else { continue };
        let sessions = app.sessions.read().await.clone();
        for s in sessions.into_iter().filter(|s| s.status == SessionStatus::WaitingForAnswer) {
            let Some(rt) = app.runtimes.lock().await.get(&s.id).cloned() else {
                continue;
            };
            let (waited, judged) = {
                let activity = rt.activity.lock().await;
                let waited = activity
                    .question_since
                    .map(|since| (Utc::now() - since).num_minutes())
                    .unwrap_or(0);
                (waited, activity.judged)
            };
            if waited < judge.after_minutes as i64 {
                continue;
            }
            if judged >= judge.max_answers {
                continue;
            }
            let Some((question_id, questions)) = rt.open_question().await else {
                continue;
            };
            let task = crate::memory::task_query(&s.issue_title, None, &s.instructions);
            match judge_one(&app, &s.id, &judge, &task, &question_id, &questions).await {
                Ok(line) => {
                    app.session_log(&s.id, "info", line).await;
                    app.update_session(&s.id, |x| x.attention = None).await;
                }
                Err(e) => {
                    // Left for the person: the watchdog's own flag is what surfaces it.
                    app.session_log(&s.id, "warn", format!("autonomous: left this question for you ({e:#})"))
                        .await;
                    rt.activity.lock().await.judged = judge.max_answers;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(text: &str, labels: &[&str], multi: bool) -> Value {
        json!({
            "question": text,
            "multi_select": multi,
            "options": labels.iter().map(|l| json!({"label": l, "description": ""})).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn the_prompt_carries_the_task_and_every_option() {
        let questions = vec![question("Which database?", &["Postgres", "SQLite"], false)];
        let p = prompt("Add a users table", &questions);
        assert!(p.contains("Add a users table"));
        assert!(p.contains("Which database?"));
        assert!(p.contains("- Postgres"));
        assert!(p.contains("- SQLite"));
        assert!(p.contains("JSON only"));
    }

    #[test]
    fn a_chosen_label_has_to_be_one_that_was_offered() {
        let questions = vec![question("Which database?", &["Postgres", "SQLite"], false)];
        let (answers, reason) = decide(
            r#"{"answers": {"Which database?": "Postgres"}, "reason": "already a dependency"}"#,
            &questions,
            false,
        )
        .unwrap();
        assert_eq!(answers["Which database?"], "Postgres");
        assert_eq!(reason, "already a dependency");

        // The case this check exists for: a plausible answer nobody offered.
        assert_eq!(
            decide(
                r#"{"answers": {"Which database?": "MySQL"}, "reason": "why not"}"#,
                &questions,
                false
            ),
            Err(Refusal::NotOffered("MySQL".into()))
        );
        // And a question the judge skipped.
        assert_eq!(
            decide(r#"{"answers": {}, "reason": "no idea"}"#, &questions, false),
            Err(Refusal::Incomplete("Which database?".into()))
        );
    }

    #[test]
    fn free_text_is_refused_unless_it_is_switched_on() {
        let questions = vec![question("What should the table be called?", &[], false)];
        let reply = r#"{"answers": {"What should the table be called?": "users"}, "reason": "matches the task"}"#;
        assert_eq!(
            decide(reply, &questions, false),
            Err(Refusal::FreeTextRefused("What should the table be called?".into()))
        );
        let (answers, _) = decide(reply, &questions, true).unwrap();
        assert_eq!(answers["What should the table be called?"], "users");
    }

    #[test]
    fn several_labels_are_kept_only_where_several_were_invited() {
        let multi = vec![question("Which features?", &["Auth", "Billing", "Search"], true)];
        let (answers, _) = decide(
            r#"{"answers": {"Which features?": ["Auth", "Search"]}, "reason": "the task names both"}"#,
            &multi,
            false,
        )
        .unwrap();
        assert_eq!(answers["Which features?"], json!(["Auth", "Search"]));

        // One of them is not offered, so none of it is an answer.
        assert_eq!(
            decide(
                r#"{"answers": {"Which features?": ["Auth", "Telemetry"]}, "reason": "…"}"#,
                &multi,
                false
            ),
            Err(Refusal::NotOffered("Telemetry".into()))
        );
    }

    #[test]
    fn prose_around_the_json_is_tolerated_but_prose_instead_of_it_is_not() {
        let questions = vec![question("Which database?", &["Postgres"], false)];
        let chatty =
            "Sure — here is my answer:\n```json\n{\"answers\": {\"Which database?\": \"Postgres\"}, \"reason\": \"fine\"}\n```\n";
        assert!(decide(chatty, &questions, false).is_ok());
        assert_eq!(decide("I would pick Postgres.", &questions, false), Err(Refusal::Unparsable));
    }

    #[test]
    fn autonomous_mode_is_off_until_it_has_a_model() {
        use crate::config::ModuleChoice;
        let mut modules = ModulesConfig::default();
        assert!(judge(&modules, &[]).is_none(), "off by default");

        modules.autonomy = Some(ModuleChoice {
            provider: "judge".into(),
            enabled: true,
            settings: Map::new(),
        });
        assert!(judge(&modules, &[]).is_none(), "no model picked yet");

        let mut settings = Map::new();
        settings.insert("model".into(), json!("fable"));
        modules.autonomy = Some(ModuleChoice {
            provider: "judge".into(),
            enabled: true,
            settings: settings.clone(),
        });
        let picked = judge(&modules, &[]).expect("configured");
        assert_eq!(picked.model, "fable");
        assert!(!picked.free_text, "free text stays off unless asked for");

        modules.autonomy = Some(ModuleChoice {
            provider: "judge".into(),
            enabled: false,
            settings,
        });
        assert!(judge(&modules, &[]).is_none(), "switched off");
    }
}
