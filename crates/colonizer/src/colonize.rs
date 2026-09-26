//! Colonize: the cockpit's one entry point for sending colonies out. Its pane lists the scope's open
//! issues to hand off, and takes free text that becomes issues first and colonies after. The two
//! routes here are the text-to-issue half; the hand-off is the ordinary `POST /api/sessions`.
//!
//! - `POST /api/colonize/draft` turns the text into one or a few issue drafts with the cheap summary
//!   model, the same [`summaries::ask_freeform`] that titles a chat. Nothing is filed: the cockpit
//!   shows the drafts for a confirm or an edit. Without a model (or with an answer that does not
//!   read) the text itself is the one draft, so the flow never dead-ends on configuration.
//! - `POST /api/repos/{owner}/{name}/issues` files one confirmed draft with the Mothership's `gh`,
//!   through the chat's own [`chat::create_github_issue`], and answers the issue's number too, so the
//!   cockpit can put it in the list and hand it off at once. The issue gets the Source module's
//!   include labels, so the filtered list still offers it after a reload; the draft's answer names
//!   them for the confirm step. The activity layer records it as `colonize.issue`.

use crate::{ApiResult, Shared, chat, client_error, summaries};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The most drafts one text becomes; more than this is a plan, not a handful of tasks.
pub const MAX_DRAFTS: usize = 5;
/// The longest text the pane sends to be drafted.
pub const MAX_TEXT: usize = 8 * 1024;
/// A drafted title is clipped to this many characters.
const MAX_TITLE: usize = 120;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DraftRequest {
    pub text: String,
    /// The repository the issues are for, when the pane has settled on one; context for the model.
    pub repo: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Draft {
    pub title: String,
    pub body: String,
}

/// What the model is asked. Pure, for the tests.
pub fn draft_prompt(text: &str, repo: Option<&str>) -> String {
    let repo = repo.map(|r| format!(" in the repository {r}")).unwrap_or_default();
    format!(
        "Turn the request below into GitHub issues{repo} that a coding agent will work on, one agent per issue.\n\
         Make one issue unless the request clearly asks for several independent tasks, and never more than {MAX_DRAFTS}.\n\
         Each issue has a short imperative title (under 80 characters) and a Markdown body: what to do, why, and how to tell it is done.\n\
         Keep the request's own details; do not invent requirements.\n\
         Answer with JSON only, no prose and no code fence: [{{\"title\": \"...\", \"body\": \"...\"}}]\n\n\
         <request>\n{text}\n</request>"
    )
}

/// A title as one clean line of at most [`MAX_TITLE`] characters, or `None` when nothing is left.
fn clean_title(title: &str) -> Option<String> {
    let line: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line
        .trim_matches(|c: char| c == '"' || c == '`' || c == '#' || c == '*')
        .trim();
    if line.is_empty() {
        return None;
    }
    Some(if line.chars().count() > MAX_TITLE {
        format!("{}…", line.chars().take(MAX_TITLE - 1).collect::<String>().trim_end())
    } else {
        line.to_string()
    })
}

/// The drafts in a model's answer: a JSON array of `{title, body}` (or `{"issues": [...]}`), found
/// even when wrapped in prose or a code fence. Titles are cleaned, empty ones dropped, at most
/// [`MAX_DRAFTS`] kept. `None` when nothing usable is there. Pure, for the tests.
pub fn parse_drafts(answer: &str) -> Option<Vec<Draft>> {
    let from_array = || {
        let (start, end) = (answer.find('[')?, answer.rfind(']')?);
        (start < end)
            .then(|| serde_json::from_str::<Vec<Draft>>(&answer[start..=end]).ok())
            .flatten()
    };
    let from_object = || {
        let (start, end) = (answer.find('{')?, answer.rfind('}')?);
        let value: Value = serde_json::from_str(answer.get(start..=end)?).ok()?;
        serde_json::from_value::<Vec<Draft>>(value.get("issues")?.clone()).ok()
    };
    let drafts: Vec<Draft> = from_array()
        .or_else(from_object)?
        .into_iter()
        .filter_map(|d| {
            Some(Draft {
                title: clean_title(&d.title)?,
                body: d.body.trim().to_string(),
            })
        })
        .take(MAX_DRAFTS)
        .collect();
    (!drafts.is_empty()).then_some(drafts)
}

/// The text itself as one draft: its first line as the title, all of it as the body. What the pane
/// gets when no model can draft. Pure, for the tests.
pub fn fallback_draft(text: &str) -> Draft {
    let text = text.trim();
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    Draft {
        title: clean_title(first).unwrap_or_else(|| "New task".into()),
        body: text.to_string(),
    }
}

/// `POST /api/colonize/draft`: `{text, repo?}` → `{issues: [{title, body}], model, note?, labels}`.
/// `model` is null and `note` says why when the text came back as its own single draft; `labels` are
/// the Source include labels filing will add.
pub async fn draft(State(app): State<Shared>, Json(req): Json<DraftRequest>) -> ApiResult<Value> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err(client_error(StatusCode::BAD_REQUEST, "nothing to draft"));
    }
    if text.len() > MAX_TEXT {
        return Err(client_error(StatusCode::PAYLOAD_TOO_LARGE, "the text is over 8 KB"));
    }
    if let Some(repo) = req.repo.as_deref()
        && !crate::util::valid_repo(repo)
    {
        return Err(client_error(StatusCode::BAD_REQUEST, "invalid repository name"));
    }
    let prompt = draft_prompt(text, req.repo.as_deref());
    let mut answer = match summaries::ask_freeform(&app, &prompt).await {
        Ok((answer, model)) => match parse_drafts(&answer) {
            Some(issues) => json!({"issues": issues, "model": model}),
            None => json!({
                "issues": [fallback_draft(text)],
                "model": null,
                "note": format!("{model} gave no drafts that read; the text is the draft"),
            }),
        },
        Err(e) => json!({"issues": [fallback_draft(text)], "model": null, "note": e}),
    };
    answer["labels"] = json!(crate::github::source_include_labels(&app).await);
    Ok(Json(answer))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NewRepoIssue {
    pub title: String,
    pub body: String,
}

/// The issue number at the end of a `gh issue create` URL. Pure, for the tests.
pub fn issue_number(url: &str) -> Option<u64> {
    let (rest, last) = url.trim_end_matches('/').rsplit_once('/')?;
    rest.ends_with("/issues").then(|| last.parse().ok()).flatten()
}

/// `POST /api/repos/{owner}/{name}/issues`: files `{title, body}` on the repository and answers
/// `{repo, number, title, url, labels, labels_skipped}`: the Source labels the issue carries, and
/// any it could not be given (the issue is filed either way).
pub async fn create_issue(
    State(app): State<Shared>,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<NewRepoIssue>,
) -> ApiResult<Value> {
    let repo = format!("{owner}/{name}");
    let issue = chat::NewIssue {
        repo: repo.clone(),
        title: req.title,
        body: req.body,
    };
    let filed = chat::create_github_issue(&app, "colonize", &issue).await?;
    Ok(Json(json!({
        "repo": repo,
        "number": issue_number(&filed.url),
        "title": issue.title.trim(),
        "url": filed.url,
        "labels": filed.labels,
        "labels_skipped": filed.skipped,
    })))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/repos/{owner}/{name}/issues", routing::post(create_issue))
        .route("/api/colonize/draft", routing::post(draft))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_are_read_from_an_array_an_object_or_a_fenced_answer() {
        let one = parse_drafts(r#"[{"title": "Fix the login", "body": "It 500s."}]"#).unwrap();
        assert_eq!(
            one,
            vec![Draft {
                title: "Fix the login".into(),
                body: "It 500s.".into()
            }]
        );

        let fenced = "Here you go:\n```json\n[{\"title\":\"A\",\"body\":\"x\"},{\"title\":\"B\",\"body\":\"y\"}]\n```";
        assert_eq!(parse_drafts(fenced).unwrap().len(), 2);

        let object = r#"{"issues": [{"title": "  \"Add   dark mode\" ", "body": "  b  "}]}"#;
        assert_eq!(
            parse_drafts(object).unwrap(),
            vec![Draft {
                title: "Add dark mode".into(),
                body: "b".into()
            }]
        );
    }

    #[test]
    fn empty_titles_are_dropped_and_the_count_is_capped() {
        assert!(parse_drafts("no json here").is_none());
        assert!(parse_drafts(r#"[{"title": "  ", "body": "x"}]"#).is_none());
        let many: Vec<Value> = (0..9).map(|i| json!({"title": format!("t{i}"), "body": ""})).collect();
        assert_eq!(
            parse_drafts(&serde_json::to_string(&many).unwrap()).unwrap().len(),
            MAX_DRAFTS
        );
        let long = format!(r#"[{{"title": "{}", "body": ""}}]"#, "w".repeat(300));
        assert_eq!(parse_drafts(&long).unwrap()[0].title.chars().count(), MAX_TITLE);
    }

    #[test]
    fn the_fallback_is_the_text_itself() {
        let d = fallback_draft("\n  Make the export a zip\nwith the images beside it  ");
        assert_eq!(d.title, "Make the export a zip");
        assert_eq!(d.body, "Make the export a zip\nwith the images beside it");
        assert_eq!(fallback_draft("   ").title, "New task");
    }

    #[test]
    fn the_prompt_names_the_repository_and_the_cap() {
        let p = draft_prompt("do it", Some("acme/web"));
        assert!(
            p.contains("in the repository acme/web")
                && p.contains("never more than 5")
                && p.contains("<request>\ndo it\n</request>")
        );
        assert!(!draft_prompt("do it", None).contains("repository"));
    }

    #[test]
    fn issue_numbers_come_from_the_url() {
        assert_eq!(issue_number("https://github.com/acme/web/issues/123"), Some(123));
        assert_eq!(issue_number("https://github.com/acme/web/issues/123/"), Some(123));
        assert_eq!(issue_number("https://github.com/acme/web/pull/123"), None);
        assert_eq!(issue_number("created"), None);
    }
}
