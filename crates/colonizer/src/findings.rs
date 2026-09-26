//! Findings: something a colony noticed outside its task, confirmed by its orchestrator, filed as a
//! GitHub issue on the colony's repository (docs/protocol.md §6.6).
//!
//! The colony only proposes. Filing happens here, on the host, because the GitHub token never
//! enters a colony — and because a colony's output is untrusted, everything it sends is bounded,
//! capped per colony and checked against open issues before anything is created.

use crate::{
    ApiResult, App, Shared, client_error,
    config::CoAuthor,
    sessions::Session,
    util::{exec, truncate},
};
use anyhow::{Result, bail};
use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

/// The label filed findings carry, so they can be found and triaged together.
pub const LABEL: &str = "colonizer-finding";
/// More than this from one colony is a colony that has lost the plot, or been talked into spam.
pub const MAX_PER_COLONY: usize = 5;

const MAX_TITLE: usize = 200;
const MAX_BODY: usize = 20_000;
const MAX_EVIDENCE: usize = 5_000;

#[derive(Debug, Clone, PartialEq)]
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
    normalize_title(title).split(' ').take(12).collect::<Vec<_>>().join(" ")
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
pub fn issue_body(finding: &Finding, s: &Session, co_author: Option<&CoAuthor>) -> String {
    let origin = match s.issue {
        Some(n) => format!("while working on #{n}"),
        None => "during an open session on this repository".to_string(),
    };
    // A markdown link, not a bare @mention, so filing does not ping the account.
    let credit = match co_author.and_then(|who| who.github_login()) {
        Some(login) => format!(", credited to [@{login}](https://github.com/{login})"),
        None => String::new(),
    };
    format!(
        "{}\n\n### How it was confirmed\n\n{}\n\n---\n\n<sub>Found by a [Colonizer](https://colonizer.dev) colony {origin}{credit}, and confirmed by the colony's orchestrator before filing. It is outside that task, so nothing here has been changed. Colony `{}`.</sub>\n",
        finding.body, finding.evidence, s.id
    )
}

/// How many findings a colony has already filed or matched, from its record on disk.
///
/// Only the line that filed or matched counts: a finding that was validated but never filed consumed
/// nothing, so a colony that keeps submitting junk is stopped by the cap while one whose findings the
/// orchestrator rejects is not punished for trying. The later stages of a filed finding — its fix
/// colony (which carries the `issue` again), review, merge, or an error along the way — are the same
/// finding, so they do not count a second time. A legacy line with no `state` counts when it carries
/// `issue` or `duplicate_of`, which is how filing was recorded before states existed.
pub fn count(record: &Path) -> usize {
    std::fs::read_to_string(record)
        .map(|content| {
            content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter(|line| serde_json::from_str::<Value>(line).ok().is_some_and(|v| used_the_cap(&v)))
                .count()
        })
        .unwrap_or(0)
}

/// Whether one ledger line is the one that filed or matched its finding.
fn used_the_cap(line: &Value) -> bool {
    match line.get("state").and_then(Value::as_str) {
        Some(state) => matches!(state, "filed" | "duplicate"),
        None => line.get("issue").is_some() || line.get("duplicate_of").is_some(),
    }
}

/// One line of a session's findings ledger (`sessions/<id>/findings.jsonl`), read back. The ledger
/// is append-only and one line per stage transition, so a finding appears several times; the
/// `state` field says which stage. Lines written before states existed carry no `state` — a present
/// `issue` or `duplicate_of` *is* the state, so `records` infers it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FindingRecord {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<String>,
    /// Which session's ledger this line came from; the ledger itself does not say, the reader does.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// Filled only by the aggregate reader, so the per-session view stays the bare ledger line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// Reads a session's findings ledger, in the order it was written, tolerating whatever a crash or
/// an older version left behind: a torn or corrupt line costs itself, and a legacy line without
/// `state` is read by its `issue` or `duplicate_of` field.
pub fn records(path: &Path) -> Vec<FindingRecord> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<FindingRecord>(line).ok())
        .filter(|rec| !rec.title.is_empty())
        .map(|mut rec| {
            if rec.state.is_none() {
                // A legacy line: writing an issue *was* the filment, matching an open one the
                // duplicate check, so the state is read off what the line carries.
                rec.state = if rec.issue.is_some() {
                    Some("filed".into())
                } else if rec.duplicate_of.is_some() {
                    Some("duplicate".into())
                } else {
                    None
                };
            }
            rec
        })
        .collect()
}

/// The finding ledger of one session, each line on the side of the colony it records.
pub async fn list(State(app): State<Shared>, AxumPath(id): AxumPath<String>) -> ApiResult<Vec<FindingRecord>> {
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let record = app.session_dir(&id).join("findings.jsonl");
    let mut out = records(&record);
    for line in &mut out {
        line.session = id.clone();
    }
    Ok(Json(out))
}

/// The finding ledger of every session, newest session first with each line naming its colony.
pub async fn list_all(State(app): State<Shared>) -> Json<Vec<FindingRecord>> {
    let sessions = app.sessions.read().await.clone();
    let mut out = Vec::new();
    for s in sessions.into_iter().rev() {
        let record = app.session_dir(&s.id).join("findings.jsonl");
        let mut lines = records(&record);
        for line in &mut lines {
            line.session = s.id.clone();
            line.repo = Some(s.repo.clone());
        }
        out.extend(lines);
    }
    Json(out)
}

/// Files `finding` on the colony's repository unless an open issue already has its title.
pub async fn file(app: &App, s: &Session, finding: &Finding, body_path: &Path) -> Result<Filed> {
    // Issue #84: fails closed here too, not only at the caller, so no path to `gh label create` or
    // `gh issue create` skips the operator's kill-switch.
    if crate::authority::external_writes_blocked() {
        bail!("refusing to file a finding: external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS / COLONIZER_NO_WRITE)");
    }
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
        if let Some(url) = duplicate_of(&finding.title, &serde_json::from_str(&open).unwrap_or(json!([]))) {
            return Ok(Filed::Duplicate(url));
        }
    }

    let co_author = crate::config::FileConfig::load(&app.cfg.config_dir).publish.co_author;
    std::fs::write(body_path, issue_body(finding, s, co_author.as_ref()))?;
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
    let url = out.lines().rev().find(|l| l.starts_with("https://")).unwrap_or(out.trim());
    Ok(Filed::Issue(truncate(url, 500)))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/sessions/{id}/findings", routing::get(list))
        .route("/api/findings", routing::get(list_all))
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
    fn the_record_counts_only_lines_that_filed_or_matched() {
        let dir = std::env::temp_dir().join(format!("colonizer-findings-count-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("findings.jsonl");
        assert_eq!(count(&record), 0, "no record yet");
        std::fs::write(
            &record,
            concat!(
                "{\"title\":\"a\",\"issue\":\"https://x/1\"}\n",
                "{\"title\":\"b\",\"state\":\"duplicate\",\"duplicate_of\":\"https://x/2\"}\n",
                "{\"title\":\"c\",\"state\":\"validated\",\"severity\":\"high\"}\n",
                "{\"title\":\"d\",\"state\":\"rejected\",\"reason\":\"not a bug\"}\n",
                "\n",
            ),
        )
        .unwrap();
        // A colony that files two findings is at 2 of its 5, however many more it validated or
        // had rejected in between: those stages never consume the cap.
        assert_eq!(count(&record), 2);
        // A filed finding's later stages are the same finding: the fix colony line carries `issue`
        // again, but neither it nor the review, the merge or an error uses another slot.
        std::fs::write(
            &record,
            concat!(
                "{\"title\":\"e\",\"state\":\"validated\",\"severity\":\"high\"}\n",
                "{\"title\":\"e\",\"state\":\"filed\",\"issue\":\"https://x/3\"}\n",
                "{\"title\":\"e\",\"state\":\"fix_colony\",\"fix_session\":\"f\",\"issue\":\"https://x/3\"}\n",
                "{\"title\":\"e\",\"state\":\"review\",\"review_session\":\"r\",\"verdict\":\"approve\",\"pr\":\"https://x/pull/4\"}\n",
                "{\"title\":\"e\",\"state\":\"merged\",\"pr\":\"https://x/pull/4\"}\n",
                "{\"title\":\"f\",\"state\":\"error\",\"reason\":\"could not start the fix colony\"}\n",
            ),
        )
        .unwrap();
        assert_eq!(count(&record), 1, "one filed finding, however far its fix got");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn records_reads_a_mixed_ledger_in_order_and_infers_legacy_states() {
        let dir = std::env::temp_dir().join(format!("colonizer-findings-records-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("findings.jsonl");
        std::fs::write(
            &record,
            concat!(
                // The lines an older build wrote: no state, the issue field *was* the filment.
                "{\"title\":\"legacy\",\"issue\":\"https://x/1\"}\n",
                "not json at all\n",
                "{\"brand_new\":true}\n",
                "{\"title\":\"watched\",\"state\":\"validated\",\"severity\":\"critical\"}\n",
                "{\"title\":\"held\",\"state\":\"rejected\",\"reason\":\"spurious\"}\n",
                "{\"title\":\"legacy dup\",\"duplicate_of\":\"https://x/2\"}\n",
            ),
        )
        .unwrap();
        let lines = records(&record);
        let states: Vec<Option<String>> = lines.iter().map(|l| l.state.clone()).collect();
        assert_eq!(
            states,
            vec![
                Some("filed".into()),
                Some("validated".into()),
                Some("rejected".into()),
                Some("duplicate".into()),
            ],
            "a corrupt or shapeless line costs itself, and legacy lines read by what they carry"
        );
        assert_eq!(lines[0].issue.as_deref(), Some("https://x/1"));
        assert_eq!(lines[1].severity.as_deref(), Some("critical"));
        assert_eq!(lines[2].reason.as_deref(), Some("spurious"));
        assert_eq!(lines[3].duplicate_of.as_deref(), Some("https://x/2"));
        assert!(records(&dir.join("nowhere")).is_empty(), "no ledger is no ledger");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Issue #84: `file` itself refuses while external writes are blocked, before any `gh` call.
    #[tokio::test]
    async fn filing_refuses_while_external_writes_are_blocked() {
        let root = std::env::temp_dir().join(format!("colonizer-findings-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let s = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
        let finding = Finding {
            title: "A title".into(),
            body: "A body".into(),
            evidence: "Checked".into(),
        };
        let _blocked = crate::authority::test_block_external_writes();
        let err = file(&app, &s, &finding, &root.join("body.md")).await.unwrap_err();
        assert!(format!("{err:#}").contains("external writes are blocked"), "{err:#}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_findings_footer_credits_the_configured_co_author_once() {
        let finding = Finding {
            title: "A title".into(),
            body: "A body".into(),
            evidence: "Checked".into(),
        };
        let mut s = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::Running);
        s.id = "abc123".into();
        s.issue = Some(7);
        let settlers = CoAuthor::settlers();
        let out = issue_body(&finding, &s, Some(&settlers));
        let credit = "[@colonizer-settlers](https://github.com/colonizer-settlers)";
        assert_eq!(out.matches(credit).count(), 1, "{out}");
        assert_eq!(out.matches("<sub>").count(), 1, "still one footer: {out}");
        assert!(
            out.contains(&format!("colony while working on #7, credited to {credit}, and confirmed by")),
            "{out}"
        );
        // Off, or an address with no GitHub account behind it, means no credit clause.
        let off = issue_body(&finding, &s, None);
        assert!(!off.contains("credited to"), "{off}");
        assert_eq!(off.matches("<sub>").count(), 1, "{off}");
        let custom = CoAuthor {
            name: "Someone Else".into(),
            email: "someone@example.com".into(),
        };
        let plain = issue_body(&finding, &s, Some(&custom));
        assert!(!plain.contains("credited to"), "{plain}");
        assert_eq!(plain.matches("<sub>").count(), 1, "{plain}");
    }
}
