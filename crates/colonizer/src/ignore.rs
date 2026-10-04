//! Maintainer opt-out (issue #909): refuse a launch on a repo that doesn't want Colonizer.
//!
//! A repository can say it does not want colonies with any one of three signals, all read from
//! GitHub before a launch:
//! - a `.colonizer-ignore` file at the repo's root — its presence alone is the signal;
//! - a `colonizer: ignore` label (any case) on the issue being launched against;
//! - `enabled = false` under a `[colonizer]` table in `.colonizer/config.toml` at the repo's root.
//!
//! Any of them refuses the launch with a 409, unless the GitHub viewer — the account making the
//! launch — is the repo's own owner (the `owner` of `owner/repo`, any case): an owner can always
//! launch on their own repo. The caller applies that override with [`is_owner`].
//!
//! Like the epic guard (epic.rs), the lookups are best effort: a fetch that fails just means "no
//! signal found here" and lets the launch through. A missing file or label is the ordinary case and
//! is not logged; only a config file that exists but cannot be read or parsed is, via `eprintln!`.

use crate::epic::Fetch;
use anyhow::Result;

const IGNORE_FILE_PATH: &str = ".colonizer-ignore";
const CONFIG_FILE_PATH: &str = ".colonizer/config.toml";
const IGNORE_LABEL: &str = "colonizer: ignore";

/// The launch gate: `Some(message)` when the launch must be refused with a 409, before the caller
/// applies the owner override. A file or label simply not existing is the ordinary case and isn't
/// logged; a config file that exists but fails to read or parse is, and lets the launch through.
pub async fn launch_refusal(fetch: &Fetch, repo: &str, issue: Option<u64>) -> Option<String> {
    if has_ignore_file(fetch, repo).await {
        return Some(refusal_message(repo, "it has a `.colonizer-ignore` file"));
    }
    if let Some(issue) = issue
        && has_ignore_label(fetch, repo, issue).await
    {
        return Some(refusal_message(repo, &format!("issue #{issue} is labeled `{IGNORE_LABEL}`")));
    }
    match config_disables(fetch, repo).await {
        Ok(true) => Some(refusal_message(
            repo,
            "`.colonizer/config.toml` sets `enabled = false` under `[colonizer]`",
        )),
        Ok(false) => None,
        Err(e) => {
            eprintln!("colonizer-ignore: could not read {repo}'s .colonizer/config.toml ({e:#}); launching anyway");
            None
        }
    }
}

/// True when `viewer_login` is the account that owns `repo` ("owner/name") — the override that lets
/// an opted-out repo's own maintainer launch on it anyway. Only the owner segment is compared, and
/// case-insensitively, exactly as GitHub treats logins.
pub fn is_owner(repo: &str, viewer_login: &str) -> bool {
    repo.split('/')
        .next()
        .is_some_and(|owner| owner.eq_ignore_ascii_case(viewer_login))
}

/// The repo has a `.colonizer-ignore` file at its root (its mere presence opts out).
async fn has_ignore_file(fetch: &Fetch, repo: &str) -> bool {
    fetch(format!("repos/{repo}/contents/{IGNORE_FILE_PATH}")).await.is_ok()
}

/// The issue carries a `colonizer: ignore` label. Either label shape GitHub answers with (`{name}`
/// objects or bare strings) is tolerated, like `epic::label_names`.
async fn has_ignore_label(fetch: &Fetch, repo: &str, issue: u64) -> bool {
    let Ok(labels) = fetch(format!("repos/{repo}/issues/{issue}/labels")).await else {
        return false;
    };
    labels.as_array().is_some_and(|ls| {
        ls.iter().any(|l| {
            l["name"]
                .as_str()
                .or_else(|| l.as_str())
                .is_some_and(|name| name.trim().eq_ignore_ascii_case(IGNORE_LABEL))
        })
    })
}

/// Whether `.colonizer/config.toml` exists and sets `enabled = false` under `[colonizer]`. A missing
/// file is `Ok(false)`; a file that exists but cannot be decoded or parsed is an `Err`, which the
/// gate logs and lets through.
async fn config_disables(fetch: &Fetch, repo: &str) -> Result<bool> {
    let Ok(file) = fetch(format!("repos/{repo}/contents/{CONFIG_FILE_PATH}")).await else {
        return Ok(false);
    };
    let content = file["content"].as_str().unwrap_or_default().replace(['\n', '\r'], "");
    if content.is_empty() {
        return Ok(false);
    }
    let bytes =
        crate::util::b64_decode(&content).ok_or_else(|| anyhow::anyhow!("{CONFIG_FILE_PATH}'s content is not valid base64"))?;
    let text = String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("{CONFIG_FILE_PATH} is not valid UTF-8"))?;
    let value: toml::Value = toml::from_str(&text).map_err(|e| anyhow::anyhow!("could not parse {CONFIG_FILE_PATH}: {e}"))?;
    let enabled = value
        .as_table()
        .and_then(|t| t.get("colonizer"))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get("enabled"))
        .and_then(toml::Value::as_bool);
    Ok(enabled == Some(false))
}

/// The 409's words: which opt-out the repo set, and how it can be lifted.
fn refusal_message(repo: &str, reason: &str) -> String {
    format!(
        "{repo} has opted out of Colonizer: {reason}. Only the repo's own owner can launch a colony \
         here; ask the maintainer to remove the opt-out."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// A fetcher answering from a fixed map of `gh api` paths, recording what was asked.
    fn fake(answers: Vec<(&'static str, Value)>, asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Fetch {
        Box::new(move |path: String| {
            asked.lock().unwrap().push(path.clone());
            let answer = answers.iter().find(|(p, _)| *p == path).map(|(_, v)| v.clone());
            Box::pin(async move { answer.ok_or_else(|| anyhow::anyhow!("HTTP 404: Not Found ({path})")) })
        })
    }

    /// The base64 `content` field of a `.colonizer/config.toml` as GitHub's contents API returns it:
    /// wrapped at 60 columns with newlines between the chunks.
    fn config_content(text: &str) -> Value {
        let encoded = crate::util::b64_encode(text.as_bytes());
        let wrapped: String = encoded
            .as_bytes()
            .chunks(60)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        json!({"content": wrapped, "encoding": "base64"})
    }

    fn recording() -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()))
    }

    #[tokio::test]
    async fn an_ignore_file_refuses_the_launch() {
        let asked = recording();
        let fetch = fake(
            vec![("repos/acme/app/contents/.colonizer-ignore", json!({"type": "file"}))],
            asked.clone(),
        );
        let message = launch_refusal(&fetch, "acme/app", None).await.expect("the file refuses");
        assert!(message.contains("`.colonizer-ignore`"), "{message}");
        // The file is the first thing checked, so nothing else is asked.
        assert_eq!(
            *asked.lock().unwrap(),
            vec!["repos/acme/app/contents/.colonizer-ignore".to_string()]
        );
    }

    #[tokio::test]
    async fn an_ignore_label_refuses_in_any_case_but_only_when_an_issue_is_given() {
        let asked = recording();
        let fetch = fake(
            vec![(
                "repos/acme/app/issues/42/labels",
                json!([{"name": "bug"}, {"name": "Colonizer: Ignore"}]),
            )],
            asked.clone(),
        );
        let message = launch_refusal(&fetch, "acme/app", Some(42)).await.expect("the label refuses");
        assert!(message.contains("issue #42 is labeled `colonizer: ignore`"), "{message}");
        assert!(asked.lock().unwrap().contains(&"repos/acme/app/issues/42/labels".to_string()));

        // A bare-string label shape is tolerated too.
        let bare = fake(
            vec![("repos/acme/app/issues/7/labels", json!(["COLONIZER: IGNORE"]))],
            recording(),
        );
        assert!(launch_refusal(&bare, "acme/app", Some(7)).await.is_some());

        // With no issue the label is never consulted (and the missing config leaves no refusal).
        asked.lock().unwrap().clear();
        assert_eq!(launch_refusal(&fetch, "acme/app", None).await, None);
        assert!(!asked.lock().unwrap().iter().any(|p| p.contains("labels")));
    }

    #[tokio::test]
    async fn a_repo_config_with_enabled_false_refuses() {
        let fetch = fake(
            vec![(
                "repos/acme/app/contents/.colonizer/config.toml",
                config_content("[colonizer]\nenabled = false\n"),
            )],
            recording(),
        );
        let message = launch_refusal(&fetch, "acme/app", None).await.expect("the config refuses");
        assert!(
            message.contains("`.colonizer/config.toml` sets `enabled = false`"),
            "{message}"
        );

        // `enabled = true`, and a config with no `[colonizer]` table, do not refuse.
        for text in ["[colonizer]\nenabled = true\n", "[other]\nenabled = false\n", ""] {
            let fetch = fake(
                vec![("repos/acme/app/contents/.colonizer/config.toml", config_content(text))],
                recording(),
            );
            assert_eq!(launch_refusal(&fetch, "acme/app", None).await, None, "{text:?}");
        }
    }

    #[tokio::test]
    async fn a_repo_with_no_signal_launches_and_a_broken_config_does_not_block() {
        let asked = recording();
        let fetch = fake(vec![], asked.clone());
        assert_eq!(launch_refusal(&fetch, "acme/app", Some(1)).await, None);
        // No issue: file, then config. With an issue: file, label, config.
        assert_eq!(
            *asked.lock().unwrap(),
            vec![
                "repos/acme/app/contents/.colonizer-ignore".to_string(),
                "repos/acme/app/issues/1/labels".to_string(),
                "repos/acme/app/contents/.colonizer/config.toml".to_string(),
            ]
        );

        asked.lock().unwrap().clear();
        assert_eq!(launch_refusal(&fetch, "acme/app", None).await, None);
        assert_eq!(
            *asked.lock().unwrap(),
            vec![
                "repos/acme/app/contents/.colonizer-ignore".to_string(),
                "repos/acme/app/contents/.colonizer/config.toml".to_string(),
            ]
        );

        // A config file that exists but does not parse is logged and lets the launch through.
        let broken = fake(
            vec![(
                "repos/acme/app/contents/.colonizer/config.toml",
                json!({"content": "not base64 !!"}),
            )],
            recording(),
        );
        assert_eq!(launch_refusal(&broken, "acme/app", None).await, None);
    }

    #[test]
    fn is_owner_matches_the_owner_segment_case_insensitively() {
        assert!(is_owner("acme/app", "acme"));
        assert!(is_owner("acme/app", "ACME"));
        assert!(is_owner("Acme/app", "acme"));
        assert!(!is_owner("acme/app", "app"));
        assert!(!is_owner("acme/app", "someone-else"));
        assert!(!is_owner("acme/app", "acme/app"));
    }
}
