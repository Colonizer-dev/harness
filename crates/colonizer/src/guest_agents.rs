//! Guest agents: explore and judge. The judge reviews code changes and files findings when
//! configured. The explore agent handles code search and analysis within a scoped model.
//!
//! Both agents are invoked as subagents of the colony's main agent and have configurable models
//! and enabled state. The judge can review pull request changes and architectural decisions,
//! returning structured findings that the verify flow can act on.

use crate::{App, sessions::Session};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level configuration for guest agents.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct GuestAgents {
    #[serde(default)]
    pub explore: ExploreConfig,
    #[serde(default)]
    pub judge: JudgeConfig,
}

/// Configuration for the explore guest agent.
///
/// The explore agent performs read-only code search and analysis using a restricted model.
/// It can locate files, understand architecture, and answer questions about the codebase
/// without making changes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ExploreConfig {
    /// Whether the explore agent is enabled. Defaults to true.
    #[serde(default = "default_explore_enabled")]
    pub enabled: Option<bool>,
    /// The model to use for exploration. Defaults to Haiku for cost efficiency.
    #[serde(default)]
    pub model: Option<String>,
}

/// Configuration for the judge guest agent.
///
/// The judge reviews code changes, architectural decisions, and findings before they are
/// acted upon. It can validate that changes are safe and aligned with repository standards.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct JudgeConfig {
    /// Whether the judge agent is enabled. Defaults to false (must be explicitly enabled).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// The model to use for judgment. Defaults to Opus for advanced reasoning.
    #[serde(default)]
    pub model: Option<String>,
}

/// Default for explore being enabled.
fn default_explore_enabled() -> Option<bool> {
    Some(true)
}

/// The result of a judgment: a finding about code quality, correctness, or design.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Brief one-line summary of what was found.
    pub title: String,
    /// Detailed explanation of the finding, why it matters, and suggested resolution.
    pub body: String,
    /// How the finding was discovered or confirmed by the judge.
    pub evidence: String,
    /// Severity level: "error", "warning", or "info".
    #[serde(default = "default_severity")]
    pub severity: String,
}

fn default_severity() -> String {
    "warning".to_string()
}

impl Default for GuestAgents {
    fn default() -> Self {
        Self {
            explore: ExploreConfig::default(),
            judge: JudgeConfig::default(),
        }
    }
}

impl Default for ExploreConfig {
    fn default() -> Self {
        Self {
            enabled: Some(true),
            model: Some("claude-haiku-4-5".to_string()),
        }
    }
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            enabled: Some(false),
            model: Some("claude-opus-4-1".to_string()),
        }
    }
}

/// The guest agents feature registration. Currently has no routes or activity kinds,
/// but is registered as a feature for consistency with other configurable modules.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "guest_agents",
    routes: routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

/// Guest agents currently has no routes. This function is required by the feature registration.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    axum::Router::new()
}

/// Invokes the judge agent to review code changes and return findings.
///
/// This function checks if the judge is enabled in the organization settings, then
/// launches the judge agent with the provided context (session, optional diff, and changes).
///
/// # Arguments
///
/// * `app` - The main application state
/// * `session` - The current session context
/// * `diff` - Optional unified diff of changes to review
/// * `changes` - Optional structured changes to review
///
/// # Returns
///
/// A vector of findings, or an empty vector if the judge is disabled or an error occurs.
/// In the stub implementation, this returns an empty vector.
pub async fn invoke_judge(
    _app: &App,
    _session: &Session,
    _diff: Option<String>,
    _changes: Option<Value>,
) -> Result<Vec<Finding>> {
    // Stub implementation: returns empty findings.
    // Full implementation will:
    // 1. Check if judge is enabled in org settings
    // 2. Prepare a prompt with the diff/changes and context
    // 3. Call the Claude API with the judge model
    // 4. Parse findings from the response
    // 5. Handle API errors and rate limiting gracefully
    Ok(vec![])
}

/// Formats judge findings as a message to send back to a session or colony.
///
/// This function takes a vector of findings and produces a human-readable markdown message
/// that can be sent as a session message or logged for the user to review.
///
/// # Arguments
///
/// * `findings` - The findings to format
///
/// # Returns
///
/// A markdown-formatted message summarizing all findings.
pub fn judge_findings_message(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "No issues found by the judge.".to_string();
    }

    let mut message = format!("Judge findings: {} issue(s) found\n\n", findings.len());

    for (i, finding) in findings.iter().enumerate() {
        message.push_str(&format!(
            "### {}: {} ({})\n\n{}\n\n**Evidence:** {}\n\n",
            i + 1,
            finding.title,
            finding.severity.to_uppercase(),
            finding.body,
            finding.evidence
        ));
    }

    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_configs() {
        let explore = ExploreConfig::default();
        assert_eq!(explore.enabled, Some(true));
        assert_eq!(explore.model, Some("claude-haiku-4-5".to_string()));

        let judge = JudgeConfig::default();
        assert_eq!(judge.enabled, Some(false));
        assert_eq!(judge.model, Some("claude-opus-4-1".to_string()));
    }

    #[test]
    fn test_judge_findings_message_empty() {
        let findings = vec![];
        let msg = judge_findings_message(&findings);
        assert_eq!(msg, "No issues found by the judge.");
    }

    #[test]
    fn test_judge_findings_message_with_findings() {
        let findings = vec![Finding {
            title: "Unsafe pointer usage".to_string(),
            body: "Function uses raw pointers without safety guards.".to_string(),
            evidence: "Reviewed code in unsafe block.".to_string(),
            severity: "error".to_string(),
        }];
        let msg = judge_findings_message(&findings);
        assert!(msg.contains("1 issue(s) found"));
        assert!(msg.contains("Unsafe pointer usage"));
        assert!(msg.contains("ERROR"));
    }

    #[test]
    fn test_default_guest_agents() {
        let config = GuestAgents::default();
        assert_eq!(config.explore.enabled, Some(true));
        assert_eq!(config.judge.enabled, Some(false));
    }
}
