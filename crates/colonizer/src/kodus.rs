//! Kodus integration for code review (issue #1280, https://github.com/kodustech/kodus-ai, AGPL-3.0).
//! The Kodus service is self-hosted by the operator; Colonizer stores its URL and API token in
//! Secrets and never vendors or patches Kodus itself.

use crate::open_code_review::Finding;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Kodus issue from the API response.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Issue {
    file: String,
    line: u64,
    #[serde(default)]
    end_line: Option<u64>,
    severity: String,
    #[serde(default)]
    category: Option<String>,
    message: String,
    #[serde(default)]
    suggestion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recommendation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fixable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

/// Kodus response from `POST /cli/review`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewResponse {
    summary: String,
    issues: Vec<Issue>,
    #[serde(default)]
    files_analyzed: u64,
    #[serde(default)]
    duration: u64,
}

/// Parses a Kodus response and converts issues to Findings, redacting secret-like strings.
pub(crate) fn parse_kodus_response(response: ReviewResponse) -> Vec<Finding> {
    response
        .issues
        .into_iter()
        .map(|issue| {
            let content = crate::redact::redact_text(&issue.message).into_owned();
            Finding {
                path: issue.file,
                content,
                start_line: Some(issue.line),
                end_line: issue.end_line,
                category: issue.category,
                severity: Some(issue.severity),
                suggestion_code: issue.suggestion.map(|s| crate::redact::redact_text(&s).into_owned()),
            }
        })
        .collect()
}

/// Calls the Kodus API and returns the parsed findings, with a ten-minute timeout like ocr.
pub(crate) async fn call_kodus(url: &str, team_key: &str, diff: &str) -> Result<Vec<Finding>> {
    let client = reqwest::Client::builder()
        .timeout(crate::open_code_review::REVIEW_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to create HTTP client")?;

    let endpoint = format!("{}/cli/review", url.trim_end_matches('/'));
    let body = json!({ "diff": diff });

    let response = client
        .post(&endpoint)
        .header("X-Team-Key", team_key)
        .json(&body)
        .send()
        .await
        .context("Kodus request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        // Do not log the team_key in error messages.
        bail!("Kodus returned {}", status);
    }

    let review: ReviewResponse = response.json().await.context("Kodus response was not valid JSON")?;

    Ok(parse_kodus_response(review))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spliced together at compile time so the source never holds a key-shaped literal; at
    /// runtime it is an ordinary key and trips the redactor exactly as a real one would.
    fn anthropic_key_fixture() -> &'static str {
        concat!("sk-", "ant-", "api03-", "AbCdEf123456_GhIjKl-", "789012MnOpQr")
    }

    #[test]
    fn parse_kodus_response_converts_issues_to_findings() {
        let response = ReviewResponse {
            summary: "Found 1 issue".into(),
            issues: vec![Issue {
                file: "src/lib.rs".into(),
                line: 10,
                end_line: Some(12),
                severity: "high".into(),
                category: Some("correctness".into()),
                message: "Invalid usage of function".into(),
                suggestion: Some("Use foo instead of bar".into()),
                recommendation: None,
                rule_id: None,
                fixable: None,
                fix: None,
            }],
            files_analyzed: 1,
            duration: 500,
        };

        let findings = parse_kodus_response(response);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "src/lib.rs");
        assert_eq!(findings[0].start_line, Some(10));
        assert_eq!(findings[0].end_line, Some(12));
        assert_eq!(findings[0].severity, Some("high".into()));
        assert_eq!(findings[0].category, Some("correctness".into()));
    }

    #[test]
    fn parse_kodus_response_redacts_secrets() {
        let response = ReviewResponse {
            summary: "Found 1 issue".into(),
            issues: vec![Issue {
                file: "config.py".into(),
                line: 5,
                end_line: None,
                severity: "critical".into(),
                category: Some("security".into()),
                message: format!("Found API key: {}", anthropic_key_fixture()),
                suggestion: Some("Remove the key from code".into()),
                recommendation: None,
                rule_id: None,
                fixable: None,
                fix: None,
            }],
            files_analyzed: 1,
            duration: 200,
        };

        let findings = parse_kodus_response(response);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].content.contains("[REDACTED"));
        assert!(!findings[0].content.contains(concat!("sk-", "ant")));
    }

    #[tokio::test]
    async fn kodus_api_call_with_invalid_status() {
        let server = axum::Router::new()
            .route(
                "/cli/review",
                axum::routing::post(|| async {
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        axum::Json(json!({"error": "Invalid key"})),
                    )
                }),
            )
            .into_make_service();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, server).await.ok();
        });

        let result = call_kodus(&format!("http://{}", addr), "kodus_test_key", "diff content").await;

        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(!err_msg.contains("kodus_test_key"));
    }

    #[tokio::test]
    async fn kodus_api_call_success() {
        let server = axum::Router::new()
            .route(
                "/cli/review",
                axum::routing::post(|| async {
                    (
                        axum::http::StatusCode::OK,
                        axum::http::header::HeaderMap::default(),
                        r#"{"summary":"No issues found","issues":[],"filesAnalyzed":1,"duration":100}"#,
                    )
                }),
            )
            .into_make_service();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, server).await.ok();
        });

        let findings = call_kodus(&format!("http://{}", addr), "kodus_team_key", "diff content").await;

        assert!(findings.is_ok());
        assert_eq!(findings.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn kodus_api_call_success_with_findings() {
        let server = axum::Router::new()
            .route(
                "/cli/review",
                axum::routing::post(|| async {
                    (
                        axum::http::StatusCode::OK,
                        axum::http::header::HeaderMap::default(),
                        r#"{"summary":"Found issues","issues":[{"file":"src/lib.rs","line":10,"endLine":12,"severity":"high","category":"correctness","message":"Invalid usage","suggestion":"Use foo"}],"filesAnalyzed":1,"duration":100}"#,
                    )
                }),
            )
            .into_make_service();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, server).await.ok();
        });

        let findings = call_kodus(&format!("http://{}", addr), "kodus_team_key", "diff content").await;

        assert!(findings.is_ok());
        let findings = findings.unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path, "src/lib.rs");
        assert_eq!(findings[0].start_line, Some(10));
        assert_eq!(findings[0].end_line, Some(12));
        assert_eq!(findings[0].severity, Some("high".into()));
        assert_eq!(findings[0].category, Some("correctness".into()));
    }
}
