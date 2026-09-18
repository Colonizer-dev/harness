//! Findings: something a colony noticed outside its task, confirmed by its orchestrator, filed as a
//! GitHub issue on the colony's repository (docs/protocol.md §6.6).
//!
//! The colony only proposes. Filing happens here, on the host, because the GitHub token never
//! enters a colony — and because a colony's output is untrusted, everything it sends is bounded,
//! capped per colony and checked against open issues before anything is created.

use crate::{
    App,
    sessions::Session,
    util::{exec, truncate},
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::path::Path;

/// The label filed findings carry, so they can be found and triaged together.
pub const LABEL: &str = "colonizer-finding";
/// More than this from one colony is a colony that has lost the plot, or been talked into spam.
pub const MAX_PER_COLONY: usize = 5;

const MAX_TITLE: usize = 200;
const MAX_BODY: usize = 20_000;
const MAX_EVIDENCE: usize = 5_000;

#[derive(Debug, PartialEq)]
pub struct Finding {
    pub title: String,
    pub body: String,
    pub evidence: String,
}

#[derive(Debug, PartialEq)]
pub enum Filed {
    Issue(String),
    /// An open issue already has this title; nothing was created.
    Duplicate(String),
}

/// Reads a `finding` event. Evidence is required: it is the record of the orchestrator's check.
pub fn parse(event: &Value) -> Result<Finding> {
    let field = |name: &str, max: usize| -> Result<String> {
        let value = event[name].as_str().unwrap_or_default().trim();
        if value.is_empty() {
            bail!("the finding has no {name}");
        }
        if value.chars().count() > max {
            bail!("the finding's {name} is longer than {max} characters");
        }
        Ok(value.to_string())
    };
    let title = field("title", MAX_TITLE)?;
    if title.contains('\n') {
        bail!("the finding's title spans more than one line");
    }
    Ok(Finding {
        title,
        body: field("body", MAX_BODY)?,
        evidence: field("evidence", MAX_EVIDENCE)?,
    })
}

/// Titles compared the way a person would: case, punctuation and spacing do not make an issue new.
pub fn normalize_title(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The words of a title, safe to hand to GitHub search: its qualifier syntax cannot be smuggled in.
fn search_terms(title: &str) -> String {
    normalize_title(title)
        .split(' ')
        .take(12)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The open issue among `issues` (from `gh issue list --json title,url`) with the same title, if any.
pub fn duplicate_of(title: &str, issues: &Value) -> Option<String> {
    let wanted = normalize_title(title);
    issues.as_array()?.iter().find_map(|issue| {
        (normalize_title(issue["title"].as_str()?) == wanted)
            .then(|| issue["url"].as_str().map(String::from))
            .flatten()
    })
}

/// The issue body: the finding, how it was confirmed, and where it came from.
pub fn issue_body(finding: &Finding, s: &Session) -> String {
    let origin = match s.issue {
        Some(n) => format!("while working on #{n}"),
        None => "during an open session on this repository".to_string(),
    };
    format!(
        "{}\n\n### How it was confirmed\n\n{}\n\n---\n\n<sub>Found by a [Colonizer](https://colonizer.dev) colony {origin}, and confirmed by the colony's orchestrator before filing. It is outside that task, so nothing here has been changed. Colony `{}`.</sub>\n",
        finding.body, finding.evidence, s.id
    )
}

/// How many findings a colony has already filed or matched, from its record on disk.
pub fn count(record: &Path) -> usize {
    std::fs::read_to_string(record)
        .map(|content| content.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

/// Files `finding` on the colony's repository unless an open issue already has its title.
pub async fn file(app: &App, s: &Session, finding: &Finding, body_path: &Path) -> Result<Filed> {
    let repo = s.repo.as_str();
    let terms = search_terms(&finding.title);
    if !terms.is_empty() {
        let search = format!("{terms} in:title");
        let open = exec(&mut app.gh([
            "issue",
            "list",
            "-R",
            repo,
            "--state",
            "open",
            "--search",
            search.as_str(),
            "--json",
            "title,url",
            "--limit",
            "30",
        ]))
        .await?;
        if let Some(url) = duplicate_of(
            &finding.title,
            &serde_json::from_str(&open).unwrap_or(json!([])),
        ) {
            return Ok(Filed::Duplicate(url));
        }
    }

    std::fs::write(body_path, issue_body(finding, s))?;
    // Best effort: the label may exist already, or the token may not be allowed to create labels.
    let _ = exec(&mut app.gh([
        "label",
        "create",
        LABEL,
        "-R",
        repo,
        "--color",
        "C5DEF5",
        "--description",
        "Found and confirmed by a Colonizer colony",
    ]))
    .await;
    let create = |label: bool| {
        let mut cmd = app.gh([
            "issue",
            "create",
            "-R",
            repo,
            "--title",
            finding.title.as_str(),
            "--body-file",
        ]);
        cmd.arg(body_path);
        if label {
            cmd.args(["--label", LABEL]);
        }
        cmd
    };
    let out = match exec(&mut create(true)).await {
        Ok(out) => out,
        // Filing matters more than the label: retry without it rather than lose the finding.
        Err(_) => exec(&mut create(false)).await?,
    };
    let url = out
        .lines()
        .rev()
        .find(|l| l.starts_with("https://"))
        .unwrap_or(out.trim());
    Ok(Filed::Issue(truncate(url, 500)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(title: &str, body: &str, evidence: &str) -> Value {
        json!({"type": "finding", "title": title, "body": body, "evidence": evidence})
    }

    #[test]
    fn a_finding_needs_a_title_a_body_and_the_evidence_it_was_checked() {
        let ok = parse(&event(
            "  Career pages are promised but unsupported ",
            "llms.txt says…",
            "Read model.rs",
        ))
        .unwrap();
        assert_eq!(ok.title, "Career pages are promised but unsupported");
        assert!(parse(&event("", "body", "evidence")).is_err());
        assert!(
            parse(&event("title", "body", "   ")).is_err(),
            "unconfirmed findings are not filed"
        );
        assert!(parse(&json!({"type": "finding", "title": "t", "body": "b"})).is_err());
        assert!(parse(&event("two\nlines", "body", "evidence")).is_err());
        assert!(parse(&event(&"x".repeat(MAX_TITLE + 1), "body", "evidence")).is_err());
    }

    #[test]
    fn titles_match_regardless_of_case_punctuation_and_spacing() {
        assert_eq!(
            normalize_title("  Career-pages: NOT supported! "),
            "career pages not supported"
        );
        let open = json!([
            {"title": "Something else", "url": "https://github.com/o/r/issues/1"},
            {"title": "career pages not supported", "url": "https://github.com/o/r/issues/2"},
        ]);
        assert_eq!(
            duplicate_of("Career-pages: NOT supported!", &open).as_deref(),
            Some("https://github.com/o/r/issues/2")
        );
        assert_eq!(duplicate_of("Career pages are supported", &open), None);
        assert_eq!(duplicate_of("anything", &json!({"not": "a list"})), None);
    }

    #[test]
    fn search_terms_cannot_carry_github_qualifiers() {
        assert_eq!(
            search_terms("is:closed author:someone \"quoted\""),
            "is closed author someone quoted"
        );
        assert_eq!(search_terms("!!!"), "");
    }

    #[test]
    fn the_record_counts_one_line_per_finding() {
        let dir = std::env::temp_dir().join(format!("colonizer-findings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("findings.jsonl");
        assert_eq!(count(&record), 0, "no record yet");
        std::fs::write(&record, "{\"a\":1}\n{\"b\":2}\n\n").unwrap();
        assert_eq!(count(&record), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
