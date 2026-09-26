//! Epic guard: a colony is never launched on an epic by accident.
//!
//! An epic is a planning container — its work lives in its sub-issues, each of which gets its own
//! colony. A colony launched on the epic itself redoes (or fights) those sub-issue colonies, at full
//! cost. So a launch on an issue first asks GitHub what the issue is, and refuses an epic with a 409
//! that names why it counts as one, lists its open sub-issues and names the way out:
//! `allow_epic: true`.
//!
//! An issue is an epic when any of these holds:
//! - it has sub-issues (GitHub's `sub_issues_summary.total`, or the sub-issues list itself);
//! - it carries a label named `epic` (case-insensitive);
//! - its title ends with `(epic)` or starts with `Epic:` (case-insensitive).
//!
//! Like the cross-mothership claim check (claims.rs), a failed lookup degrades to letting the launch
//! through: the guard is advice about what the issue is, not an access control.

use crate::{App, Shared, util::exec_within};
use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::time::Duration;

/// One lookup's deadline, like the claim check's: a wedged `gh` must not park a launch.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(20);
/// How many open sub-issues a refusal lists; the rest are counted.
pub const LISTED_SUB_ISSUES: usize = 10;

/// Why an issue counts as an epic, and its sub-issues as far as they are known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Epic {
    /// Plain words: "it has 5 sub-issues", "it carries the `epic` label", "its title marks it as an epic".
    pub reason: String,
    /// GitHub's sub-issue total, 0 when the issue has none (or they are unknown).
    pub sub_issues: u64,
    /// Open sub-issues as `(number, title)`, at most [`LISTED_SUB_ISSUES`].
    pub open: Vec<(u64, String)>,
    /// How many open sub-issues there are in all, beyond the listed ones.
    pub open_total: usize,
}

/// Whether a title marks its issue as an epic: ends with `(epic)` or starts with `Epic:`.
pub fn title_marks_epic(title: &str) -> bool {
    let t = title.trim().to_lowercase();
    t.ends_with("(epic)") || t.starts_with("epic:")
}

/// Label names from either shape GitHub answers with: `{name}` objects (REST, `gh issue list`) or
/// bare strings.
fn label_names(issue: &Value) -> Vec<String> {
    issue["labels"]
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .filter_map(|l| l["name"].as_str().or_else(|| l.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The sub-issue total GitHub reports on the issue itself: REST's `sub_issues_summary.total` or
/// GraphQL's `subIssuesSummary.total`. `None` when the field is absent (an older GitHub).
pub fn summary_total(issue: &Value) -> Option<u64> {
    issue["sub_issues_summary"]["total"]
        .as_u64()
        .or_else(|| issue["subIssuesSummary"]["total"].as_u64())
}

/// Why `issue` (a GitHub issue object) is an epic, or `None`. `sub_issues` is the issue's
/// sub-issue list when it was fetched; it counts even where the summary field is missing. Pure, so
/// every signal is tested directly.
pub fn detect(issue: &Value, sub_issues: Option<&Value>) -> Option<Epic> {
    let listed = sub_issues.and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let total = summary_total(issue).unwrap_or(0).max(listed.len() as u64);
    let reason = if total > 0 {
        format!("it has {total} sub-issue{}", if total == 1 { "" } else { "s" })
    } else if label_names(issue).iter().any(|l| l.trim().eq_ignore_ascii_case("epic")) {
        "it carries the `epic` label".to_string()
    } else if title_marks_epic(issue["title"].as_str().unwrap_or_default()) {
        "its title marks it as an epic".to_string()
    } else {
        return None;
    };
    let open: Vec<(u64, String)> = listed
        .iter()
        .filter(|s| !s["state"].as_str().is_some_and(|st| st.eq_ignore_ascii_case("closed")))
        .filter_map(|s| Some((s["number"].as_u64()?, s["title"].as_str().unwrap_or_default().to_string())))
        .collect();
    Some(Epic {
        reason,
        sub_issues: total,
        open_total: open.len(),
        open: open.into_iter().take(LISTED_SUB_ISSUES).collect(),
    })
}

/// The 409's words: what the issue is, why, what to launch instead, and the override.
pub fn refusal_message(issue: u64, epic: &Epic) -> String {
    let mut message = format!(
        "#{issue} is an epic ({}): an epic is a planning container, and a colony on it duplicates the \
         colonies on its sub-issues.",
        epic.reason
    );
    if epic.open.is_empty() {
        message.push_str(" Launch colonies on the issues it tracks instead.");
    } else {
        let list = epic
            .open
            .iter()
            .map(|(n, title)| format!("#{n} {}", crate::util::truncate(title, 80)))
            .collect::<Vec<_>>()
            .join("; ");
        let more = epic.open_total.saturating_sub(epic.open.len());
        let more = if more > 0 {
            format!(" (and {more} more)")
        } else {
            String::new()
        };
        message.push_str(&format!(" Launch colonies on its open sub-issues instead: {list}{more}."));
    }
    message.push_str(" Pass allow_epic to start one on the epic anyway.");
    message
}

/// The issue as the cockpit's lists carry it: `{reason, sub_issues}` or null.
pub fn marker(epic: Option<&Epic>) -> Value {
    epic.map_or(Value::Null, |e| json!({"reason": e.reason, "sub_issues": e.sub_issues}))
}

/// One GitHub REST read (`gh api <path>`), answered as JSON. Swappable so the gate is tested without
/// a network.
pub type Fetch = Box<dyn Fn(String) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

/// The real fetcher: `gh api` with the mothership's GitHub credentials.
pub fn gh_fetch(app: &Shared) -> Fetch {
    let app = app.clone();
    Box::new(move |path: String| {
        let app = app.clone();
        Box::pin(async move {
            let out = exec_within(LOOKUP_TIMEOUT, &mut app.gh(["api", path.as_str()])).await?;
            serde_json::from_str(&out).with_context(|| format!("could not parse `gh api {path}`"))
        })
    })
}

/// Looks the issue up — one read for the issue, and a second for its sub-issues only when it has
/// some (or GitHub did not say) — and answers the epic, if it is one.
pub async fn lookup(fetch: &Fetch, repo: &str, issue: u64) -> Result<Option<Epic>> {
    let found = fetch(format!("repos/{repo}/issues/{issue}")).await?;
    let subs = match summary_total(&found) {
        Some(0) => None,
        // Has sub-issues, or an older GitHub that does not summarise them: list them. A failure
        // there still leaves the summary's own count and the label/title signals.
        _ => fetch(format!("repos/{repo}/issues/{issue}/sub_issues?per_page=100"))
            .await
            .ok(),
    };
    Ok(detect(&found, subs.as_ref()))
}

/// The launch gate: `Some(message)` when the launch must be refused with a 409. `allow_epic`
/// skips the lookup entirely; a lookup that fails lets the launch through, logged.
pub async fn launch_refusal(fetch: &Fetch, repo: &str, issue: Option<u64>, allow_epic: bool) -> Option<String> {
    let issue = issue.filter(|_| !allow_epic)?;
    match lookup(fetch, repo, issue).await {
        Ok(epic) => epic.map(|e| refusal_message(issue, &e)),
        Err(e) => {
            eprintln!("epic: could not check whether #{issue} in {repo} is an epic ({e:#}); launching anyway");
            None
        }
    }
}

/// Sub-issue totals for a repository's open issues, `number → total`, from one GraphQL read per
/// hundred issues (at most two). Best effort: the cockpit's marker, not the gate.
pub async fn open_sub_issue_totals(app: &App, repo: &str) -> Result<std::collections::HashMap<u64, u64>> {
    let (owner, name) = repo.split_once('/').context("invalid repository name")?;
    const QUERY: &str = "query($owner:String!,$name:String!,$after:String){repository(owner:$owner,name:$name){\
        issues(states:OPEN,first:100,after:$after,orderBy:{field:CREATED_AT,direction:DESC}){\
        pageInfo{hasNextPage endCursor} nodes{number subIssuesSummary{total}}}}}";
    let mut totals = std::collections::HashMap::new();
    let mut after: Option<String> = None;
    for _ in 0..2 {
        let mut cmd = app.gh(["api", "graphql"]);
        cmd.args([
            "-f",
            &format!("query={QUERY}"),
            "-F",
            &format!("owner={owner}"),
            "-F",
            &format!("name={name}"),
        ]);
        if let Some(cursor) = &after {
            cmd.args(["-F", &format!("after={cursor}")]);
        }
        let out = exec_within(LOOKUP_TIMEOUT, &mut cmd).await?;
        let page: Value = serde_json::from_str(&out).context("could not parse the sub-issue totals")?;
        let issues = &page["data"]["repository"]["issues"];
        for node in issues["nodes"].as_array().into_iter().flatten() {
            if let (Some(n), Some(total)) = (node["number"].as_u64(), summary_total(node)) {
                totals.insert(n, total);
            }
        }
        match issues["pageInfo"]["endCursor"].as_str() {
            Some(cursor) if issues["pageInfo"]["hasNextPage"].as_bool() == Some(true) => after = Some(cursor.to_string()),
            _ => break,
        }
    }
    Ok(totals)
}

/// Marks each issue of a `gh issue list` answer with `epic` (see [`marker`]), from its labels,
/// its title and the sub-issue totals when they are known.
pub fn annotate(issues: Value, totals: &std::collections::HashMap<u64, u64>) -> Value {
    let Value::Array(list) = issues else { return issues };
    Value::Array(
        list.into_iter()
            .map(|mut issue| {
                let mut probe = issue.clone();
                if let Some(total) = issue["number"].as_u64().and_then(|n| totals.get(&n)) {
                    probe["sub_issues_summary"] = json!({"total": total});
                }
                issue["epic"] = marker(detect(&probe, None).as_ref());
                issue
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn sub_issues_label_and_title_each_mark_an_epic() {
        // Sub-issues, from GitHub's summary on the issue (REST or GraphQL spelling).
        let rest = json!({"title": "Remote access", "labels": [], "sub_issues_summary": {"total": 5, "completed": 0}});
        assert_eq!(detect(&rest, None).unwrap().reason, "it has 5 sub-issues");
        let graphql = json!({"title": "Remote access", "labels": [], "subIssuesSummary": {"total": 1}});
        assert_eq!(detect(&graphql, None).unwrap().reason, "it has 1 sub-issue");
        // …or from the list itself where the summary is missing (an older GitHub).
        let bare = json!({"title": "Remote access", "labels": []});
        let subs = json!([{"number": 532, "title": "Relay", "state": "open"}]);
        assert_eq!(detect(&bare, Some(&subs)).unwrap().sub_issues, 1);
        // A label named `epic`, in any case, either label shape.
        for labels in [json!([{"name": "Epic"}]), json!(["EPIC"]), json!([{"name": " epic "}])] {
            let issue = json!({"title": "Remote access", "labels": labels, "sub_issues_summary": {"total": 0}});
            assert_eq!(
                detect(&issue, None).unwrap().reason,
                "it carries the `epic` label",
                "{labels}"
            );
        }
        // A title ending in "(epic)" or starting with "Epic:".
        for title in [
            "Remote access (epic)",
            "Remote access (Epic) ",
            "Epic: remote access",
            "EPIC: remote",
        ] {
            let issue = json!({"title": title, "labels": [], "sub_issues_summary": {"total": 0}});
            assert_eq!(
                detect(&issue, None).unwrap().reason,
                "its title marks it as an epic",
                "{title}"
            );
        }
        // Plain issues are not epics: a label that only contains the word, a title that only
        // mentions it, an empty sub-issue summary.
        for issue in [
            json!({"title": "Fix the epic loader", "labels": [{"name": "epic-followup"}], "sub_issues_summary": {"total": 0}}),
            json!({"title": "Epics page is slow", "labels": [], "sub_issues_summary": {"total": 0}}),
            json!({"title": "Remote access", "labels": []}),
        ] {
            assert_eq!(detect(&issue, Some(&json!([]))), None, "{issue}");
        }
    }

    #[test]
    fn the_refusal_names_the_reason_lists_ten_open_sub_issues_and_the_override() {
        let issue = json!({"title": "Remote access (epic)", "labels": [], "sub_issues_summary": {"total": 13}});
        let subs = Value::Array(
            (0..13)
                .map(
                    |i| json!({"number": 532 + i, "title": format!("part {i}"), "state": if i == 0 { "closed" } else { "open" }}),
                )
                .collect(),
        );
        let epic = detect(&issue, Some(&subs)).unwrap();
        assert_eq!(epic.open.len(), LISTED_SUB_ISSUES);
        assert_eq!(epic.open_total, 12, "the closed one is not offered");
        assert_eq!(epic.open[0], (533, "part 1".to_string()));
        let message = refusal_message(531, &epic);
        assert!(message.starts_with("#531 is an epic (it has 13 sub-issues)"), "{message}");
        assert!(message.contains("#533 part 1; #534 part 2"), "{message}");
        assert!(!message.contains("#532 "), "closed sub-issues are not suggested: {message}");
        assert!(message.contains("(and 2 more)"), "{message}");
        assert!(
            message.ends_with("Pass allow_epic to start one on the epic anyway."),
            "{message}"
        );
        // Without a sub-issue list, the message still says what to do.
        let labelled = detect(&json!({"title": "x", "labels": ["epic"]}), None).unwrap();
        assert!(refusal_message(9, &labelled).contains("Launch colonies on the issues it tracks instead."));
    }

    /// A fetcher answering from a fixed map of `gh api` paths, recording what was asked.
    fn fake(answers: Vec<(&'static str, Value)>, asked: Arc<Mutex<Vec<String>>>) -> Fetch {
        Box::new(move |path: String| {
            asked.lock().unwrap().push(path.clone());
            let answer = answers.iter().find(|(p, _)| *p == path).map(|(_, v)| v.clone());
            Box::pin(async move { answer.ok_or_else(|| anyhow::anyhow!("HTTP 404: Not Found ({path})")) })
        })
    }

    #[tokio::test]
    async fn an_epic_launch_is_refused_unless_allow_epic_and_a_failed_lookup_lets_it_through() {
        let epic = json!({"number": 531, "title": "Remote access (epic)", "labels": [], "sub_issues_summary": {"total": 2}});
        let subs =
            json!([{"number": 532, "title": "Relay", "state": "open"}, {"number": 533, "title": "Tunnel", "state": "open"}]);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let fetch = fake(
            vec![
                ("repos/acme/app/issues/531", epic),
                ("repos/acme/app/issues/531/sub_issues?per_page=100", subs),
                (
                    "repos/acme/app/issues/532",
                    json!({"number": 532, "title": "Relay", "labels": [], "sub_issues_summary": {"total": 0}}),
                ),
            ],
            asked.clone(),
        );
        let refused = launch_refusal(&fetch, "acme/app", Some(531), false)
            .await
            .expect("an epic is refused");
        assert!(refused.contains("#531 is an epic (it has 2 sub-issues)"), "{refused}");
        assert!(refused.contains("#532 Relay; #533 Tunnel"), "{refused}");

        // The override skips the lookup altogether.
        asked.lock().unwrap().clear();
        assert_eq!(launch_refusal(&fetch, "acme/app", Some(531), true).await, None);
        assert!(asked.lock().unwrap().is_empty(), "allow_epic asks GitHub nothing");

        // A plain issue with no sub-issues costs one read, and launches.
        assert_eq!(launch_refusal(&fetch, "acme/app", Some(532), false).await, None);
        assert_eq!(*asked.lock().unwrap(), vec!["repos/acme/app/issues/532".to_string()]);

        // A launch without an issue asks nothing; a lookup that fails lets the launch through.
        asked.lock().unwrap().clear();
        assert_eq!(launch_refusal(&fetch, "acme/app", None, false).await, None);
        assert!(asked.lock().unwrap().is_empty());
        assert_eq!(launch_refusal(&fetch, "acme/app", Some(999), false).await, None);
    }

    #[test]
    fn listed_issues_carry_the_epic_marker() {
        let listed = json!([
            {"number": 531, "title": "Remote access (epic)", "labels": []},
            {"number": 540, "title": "Planning", "labels": []},
            {"number": 541, "title": "Plain bug", "labels": [{"name": "bug", "color": "d73a4a"}]},
            {"number": 542, "title": "Big thing", "labels": [{"name": "Epic", "color": "000000"}]},
        ]);
        let totals = std::collections::HashMap::from([(531, 5), (540, 3), (541, 0)]);
        let out = annotate(listed, &totals);
        assert_eq!(out[0]["epic"], json!({"reason": "it has 5 sub-issues", "sub_issues": 5}));
        assert_eq!(out[1]["epic"]["sub_issues"], 3);
        assert_eq!(out[2]["epic"], Value::Null);
        assert_eq!(out[3]["epic"]["reason"], "it carries the `epic` label");
        assert_eq!(out[3]["epic"]["sub_issues"], 0);
    }
}
