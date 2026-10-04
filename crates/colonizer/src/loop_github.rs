//! GitHub for loops (issue #778) — the contract is in docs/protocol.md §6.12. A colony has no GitHub
//! token, so a loop whose work is GitHub's is handled here: the mothership checks it can reach the
//! loop's repository before anything launches, writes the loop's inputs into the guest's read-only
//! `/colonizer/github`, and — because the colony cannot call GitHub itself — makes the writes it asks
//! for, bounded, capped and always on the colony's own repository.

use crate::{
    App, Shared,
    loops::{Loop, ORIGIN_PREFIX, loop_id_of, needs_preflight},
    sessions::{Runtime, Session, SessionLogger},
    util::exec,
};
use anyhow::{Result, bail};
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// The read-only directory a GitHub-needing loop's context lands in, as the guest sees it (`vm_dir`
/// is mounted at `/colonizer`, so nothing nested has to be mounted).
pub const CONTEXT_DIR: &str = "/colonizer/github";
/// The same directory inside the session's `vm` dir.
const DIR_NAME: &str = "github";
/// More than this from one colony is a colony that has lost the plot, or been talked into spam.
pub const MAX_ACTIONS: usize = 30;
/// Bounds on what one action may carry; the colony's output is untrusted.
const MAX_LABEL: usize = 100;
const MAX_LABELS: usize = 10;
const MAX_BODY: usize = 20_000;
/// The per-colony ledger of GitHub writes, beside `findings.jsonl`.
const LEDGER: &str = "github.jsonl";
/// Long enough for one paginated context fetch on a slow link.
const FETCH_LIMIT: Duration = Duration::from_secs(30);
/// A context fetch is not worth keeping longer than this; loop runs are minutes apart.
const FETCH_REUSE: Duration = Duration::from_secs(60);

/// RFC 3339, seconds, `Z` — never the `+00:00` offset, which a query string decodes as a space.
fn stamp(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Percent-encodes a path segment or query value: everything outside the unreserved set. A branch
/// name with a `/` and a search qualifier's `:` both have to survive the URL.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The note a preflight failure leaves on the loop: what is missing, and how to fix it. A blip is
/// named as one — the loop simply tries again — while anything else borrows
/// [`crate::github::access_error`]'s operator wording, so the one place that knows why a repository
/// read failed stays the one place.
pub async fn unreachable_note(app: &App, repo: &str, error: anyhow::Error) -> String {
    let fix = if crate::github::is_transient(&format!("{error:#}")) {
        format!("GitHub could not be reached just now for {repo}; the loop will try again on its next run")
    } else {
        crate::github::access_error(app, repo, error).await.to_string()
    };
    format!("this loop needs GitHub, but the mothership cannot reach it: {fix}")
}

/// Whether the mothership can read the loop's repository right now. A failure is the loop's to
/// report, not a boot's: it is returned to the caller, which records the note and launches nothing.
pub async fn preflight(app: &App, l: &Loop) -> Result<()> {
    if !needs_preflight(l) {
        return Ok(());
    }
    let repo = l.repo.as_str();
    match crate::github::gh_get(app, &format!("repos/{repo}"), None).await {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("{}", unreachable_note(app, repo, e).await)),
    }
}

/// The loop this session is a run of, when it is a colony loop that asked for GitHub (issue #778).
/// Both the context write and every write it later asks for are gated on this: a `github_action`
/// only ever comes from the MCP server `boot` switched on for such a run.
async fn github_loop(app: &App, s: &Session) -> Option<Loop> {
    let id = s.origin.as_deref().and_then(loop_id_of)?;
    let l = app.loops.get(id).await?;
    needs_preflight(&l).then_some(l)
}

/// Whether this session is a run of a loop that asked for GitHub; if so writes its read-only
/// context into `vm_dir` and answers true, so boot can switch the colony's tool set on. Any other
/// session answers false and writes nothing. Never fails: a fetch that goes wrong costs its one
/// file and a log line, not the boot.
pub async fn prepare(app: &Shared, s: &Session, vm_dir: &Path, log: &SessionLogger) -> bool {
    let Some(l) = github_loop(app, s).await else {
        return false;
    };
    let dir = vm_dir.join(DIR_NAME);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log.warn(format!(
            "github: could not make {}: {e:#}; the colony runs without the context",
            dir.display()
        ))
        .await;
        return false;
    }
    let since = since_for(app, &l, s).await;
    let repo = s.repo.as_str();
    // The three inputs, each narrowed server-side to what happened since the last run: the issues
    // endpoint's `since`, workflow runs' `created`, and the search API's `merged`. The window is the
    // cap; `gh_list` pages within it.
    let issues = fetch(
        app,
        &format!(
            "repos/{repo}/issues?state=open&sort=updated&direction=desc&per_page=100&since={}",
            stamp(since)
        ),
        ".[] | select(.pull_request == null) | {number, title, url: .html_url, labels: [.labels[].name], created_at, updated_at}",
    )
    .await;
    write_context(&dir, "issues.json", "issues", since, issues, log).await;
    let runs = match crate::github::default_branch(app, repo).await {
        Ok(branch) => {
            let created = encode(&format!(">={}", stamp(since)));
            fetch(
                app,
                &format!(
                    "repos/{repo}/actions/runs?branch={}&status=failure&created={created}&per_page=100",
                    encode(&branch)
                ),
                ".workflow_runs[] | {id, name, title: .display_title, url: .html_url, conclusion, head_sha, created_at}",
            )
            .await
        }
        Err(e) => Err(e),
    };
    write_context(&dir, "ci-failures.json", "runs", since, runs, log).await;
    let q = encode(&format!("repo:{repo} is:pr is:merged merged:>={}", stamp(since)));
    let merged = fetch(
        app,
        &format!("search/issues?q={q}&per_page=100&sort=updated&order=desc"),
        ".items[] | {number, title, url: .html_url, merged_at: .pull_request.merged_at, labels: [.labels[].name], user: .user.login}",
    )
    .await;
    write_context(&dir, "merged-prs.json", "pull_requests", since, merged, log).await;
    true
}

/// What "since" means for a run: the previous run of the same loop, or the 24 hours before this
/// one on a first run. Read from the sessions rather than `Loop::last_run`, which `loops::launch`
/// only records *after* `sessions::create` has already spawned this boot.
async fn since_for(app: &App, l: &Loop, s: &Session) -> DateTime<Utc> {
    let origin = format!("{ORIGIN_PREFIX}{}", l.id);
    let previous = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|o| o.id != s.id && o.origin.as_deref() == Some(origin.as_str()) && o.created_at < s.created_at)
        .map(|o| o.created_at)
        .max();
    previous.unwrap_or_else(|| s.created_at - ChronoDuration::hours(24))
}

/// One server-shaped fetch: `gh api --paginate <path> --jq <jq>`, one JSON object per line.
async fn fetch(app: &Shared, path: &str, jq: &str) -> Result<Vec<Value>> {
    let listing = crate::github::gh_list(app, path, jq, FETCH_LIMIT, FETCH_REUSE).await?;
    Ok(listing.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
}

/// Writes one context file (`{since, <key>: [...]}`), or logs why the colony goes without it.
async fn write_context(
    dir: &Path,
    name: &str,
    key: &str,
    since: DateTime<Utc>,
    fetched: Result<Vec<Value>>,
    log: &SessionLogger,
) {
    match fetched {
        Ok(items) => {
            let body =
                serde_json::to_vec_pretty(&json!({"since": stamp(since), key: items})).unwrap_or_else(|_| b"null".to_vec());
            if let Err(e) = std::fs::write(dir.join(name), body) {
                log.warn(format!("github: could not write {name}: {e:#}")).await;
            }
        }
        Err(e) => {
            log.warn(format!("github: could not fetch {name}: {e:#}; the colony runs without it"))
                .await
        }
    }
}

/// One GitHub write a colony asked for, already validated. The colony supplies issue numbers; the
/// repository is always the colony's own, never the guest's choice.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Label { issue: u64, labels: Vec<String> },
    Comment { issue: u64, body: String },
    CloseDuplicate { issue: u64, duplicate_of: u64 },
}

impl Action {
    pub fn issue(&self) -> u64 {
        match self {
            Action::Label { issue, .. } | Action::Comment { issue, .. } | Action::CloseDuplicate { issue, .. } => *issue,
        }
    }
}

/// Reads a `github_action` event into an [`Action`], refusing anything a colony should not be able
/// to ask for. Pure: what is refused is tested without GitHub.
pub fn parse_action(event: &Value) -> Result<Action> {
    let number = |name: &str| -> Result<u64> {
        let n = event[name].as_u64().unwrap_or(0);
        if n == 0 {
            bail!("{name} must be a positive issue number");
        }
        Ok(n)
    };
    let issue = number("issue")?;
    match event["tool"].as_str().unwrap_or_default() {
        "issue_label" => {
            let labels: Vec<String> = event["labels"]
                .as_array()
                .map(|labels| {
                    labels
                        .iter()
                        .filter_map(|l| l.as_str())
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            if labels.is_empty() {
                bail!("issue_label has no labels");
            }
            if labels.len() > MAX_LABELS {
                bail!("issue_label got more than {MAX_LABELS} labels");
            }
            if labels.iter().any(|l| l.chars().count() > MAX_LABEL) {
                bail!("a label is longer than {MAX_LABEL} characters");
            }
            Ok(Action::Label { issue, labels })
        }
        "issue_comment" => {
            let body = event["body"].as_str().unwrap_or_default().trim();
            if body.is_empty() {
                bail!("issue_comment has no body");
            }
            if body.chars().count() > MAX_BODY {
                bail!("the comment is longer than {MAX_BODY} characters");
            }
            Ok(Action::Comment {
                issue,
                body: body.to_string(),
            })
        }
        "issue_close_duplicate" => {
            let duplicate_of = number("duplicate_of")?;
            if duplicate_of == issue {
                bail!("an issue cannot be a duplicate of itself");
            }
            Ok(Action::CloseDuplicate { issue, duplicate_of })
        }
        other => bail!("unknown github action {other:?}"),
    }
}

/// The `gh` argv an action runs — one per command, since close-duplicate is the comment that
/// explains and then the close. Every argument is its own argv entry (never a shell string), and
/// the issue is always a validated number on the colony's own repository.
pub fn action_args(repo: &str, action: &Action) -> Vec<Vec<String>> {
    let issue = action.issue().to_string();
    let comment = |body: String| {
        vec![
            "issue".into(),
            "comment".into(),
            issue.clone(),
            "-R".into(),
            repo.into(),
            "--body".into(),
            body,
        ]
    };
    match action {
        Action::Label { labels, .. } => {
            let mut args = vec!["issue".into(), "edit".into(), issue.clone(), "-R".into(), repo.into()];
            for label in labels {
                args.push("--add-label".into());
                args.push(label.clone());
            }
            vec![args]
        }
        Action::Comment { body, .. } => vec![comment(body.clone())],
        Action::CloseDuplicate { duplicate_of, .. } => vec![
            comment(format!("Duplicate of #{duplicate_of}")),
            vec![
                "issue".into(),
                "close".into(),
                issue.clone(),
                "-R".into(),
                repo.into(),
                "--reason".into(),
                "not planned".into(),
            ],
        ],
    }
}

/// What the colony log says an action did.
pub fn describe_action(action: &Action, repo: &str) -> String {
    match action {
        Action::Label { issue, labels } => format!("labeled {repo}#{issue} {}", labels.join(", ")),
        Action::Comment { issue, .. } => format!("commented on {repo}#{issue}"),
        Action::CloseDuplicate { issue, duplicate_of } => {
            format!("closed {repo}#{issue} as a duplicate of #{duplicate_of}")
        }
    }
}

/// How many writes a colony has already made, from its ledger on disk.
pub fn count(record: &Path) -> usize {
    std::fs::read_to_string(record)
        .map(|content| content.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

/// Whether the colony has already made the most writes one colony may make.
pub fn spent_cap(record: &Path) -> bool {
    count(record) >= MAX_ACTIONS
}

fn ledger_line(action: &Action, repo: &str, outcome: &str) -> Value {
    let mut line = json!({"ts": stamp(Utc::now()), "repo": repo, "issue": action.issue(), "outcome": outcome});
    match action {
        Action::Label { labels, .. } => {
            line["tool"] = json!("issue_label");
            line["labels"] = json!(labels);
        }
        Action::Comment { .. } => line["tool"] = json!("issue_comment"),
        Action::CloseDuplicate { duplicate_of, .. } => {
            line["tool"] = json!("issue_close_duplicate");
            line["duplicate_of"] = json!(duplicate_of);
        }
    }
    line
}

fn append(record: &Path, line: &Value) {
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(record)
        && let Err(e) = writeln!(file, "{line}")
    {
        eprintln!("github: could not append to {}: {e}", record.display());
    }
}

/// A colony's GitHub write, on a spawned task like a finding: the `gh` call must not hold up the
/// event stream. Nothing is refused silently — the colony log says what happened, every time.
pub async fn perform(app: Shared, id: String, rt: std::sync::Arc<Runtime>, event: Value) {
    let Some(s) = app.session(&id).await else { return };
    if github_loop(&app, &s).await.is_none() {
        app.session_log(
            &id,
            "info",
            "ignored a github action: this colony is not a run of a GitHub loop".into(),
        )
        .await;
        return;
    }
    if crate::authority::external_writes_blocked() {
        app.session_log(
            &id,
            "info",
            "ignored a github action: external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS)".into(),
        )
        .await;
        return;
    }
    let action = match parse_action(&event) {
        Ok(action) => action,
        Err(e) => {
            app.session_log(&id, "warn", format!("ignored a github action: {e:#}")).await;
            return;
        }
    };
    let _serial = rt.github_lock.lock().await;
    let record: PathBuf = app.session_dir(&id).join(LEDGER);
    if spent_cap(&record) {
        app.session_log(
            &id,
            "warn",
            format!("ignored a github action: this colony has already made {MAX_ACTIONS}, the most one colony may"),
        )
        .await;
        return;
    }
    // Every attempt that reaches `gh` is one ledger line, whatever its outcome: a partial
    // close-duplicate (comment written, close failed) must still count against the cap.
    match run_action(&app, &s.repo, &action).await {
        Ok(()) => {
            append(&record, &ledger_line(&action, &s.repo, "ok"));
            app.session_log(&id, "info", format!("github: {}", describe_action(&action, &s.repo)))
                .await;
        }
        Err(e) => {
            let what = describe_action(&action, &s.repo);
            append(&record, &ledger_line(&action, &s.repo, "failed"));
            app.session_log(&id, "warn", format!("could not {what}: {e:#}")).await;
        }
    }
}

/// Makes the `gh` calls an action needs — close-duplicate is two: the comment, then the close.
async fn run_action(app: &App, repo: &str, action: &Action) -> Result<()> {
    for args in action_args(repo, action) {
        exec(&mut app.gh(args)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{SessionStatus, tests::app_with_colony};

    /// A GitHub colony loop, built through serde the way the store loads one.
    fn a_github_loop(id: &str) -> Loop {
        serde_json::from_value(json!({
            "id": id, "name": "Triage", "org": "acme", "repo": "acme/web", "prompt": "Triage",
            "cadence": {"every": "interval", "minutes": 60}, "kind": "colony", "needs_github": true,
            "autopilot": true, "enabled": true, "created_at": "2026-09-01T00:00:00Z"
        }))
        .unwrap()
    }

    /// A colony that is a run of that loop, the state `perform` insists on.
    async fn github_colony(id: &str) -> (Shared, std::path::PathBuf) {
        let (app, root) = app_with_colony(id, SessionStatus::Idle).await;
        app.loops.loops.write().await.push(a_github_loop("loop_a"));
        for s in app.sessions.write().await.iter_mut() {
            s.origin = Some("loop:loop_a".into());
        }
        (app, root)
    }

    #[tokio::test]
    async fn a_transient_preflight_failure_is_named_as_one_not_a_missing_token() {
        let root = std::env::temp_dir().join(format!("colonizer-github-note-{}", std::process::id()));
        let app = crate::tests::test_app(&root);
        let note = unreachable_note(&app, "acme/web", anyhow::anyhow!("gh: connection refused (timed out)")).await;
        assert!(note.contains("needs GitHub") && note.contains("acme/web"), "{note}");
        assert!(note.contains("could not be reached just now"), "{note}");
        assert!(!note.contains("Settings"), "a blip is not a missing token: {note}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_action_is_refused_unless_it_is_bounded_and_well_formed() {
        let label = parse_action(&json!({"tool": "issue_label", "issue": 42, "labels": [" bug ", "P1"]})).unwrap();
        assert_eq!(
            label,
            Action::Label {
                issue: 42,
                labels: vec!["bug".into(), "P1".into()]
            },
            "labels are trimmed"
        );
        assert!(parse_action(&json!({"tool": "issue_comment", "issue": 1, "body": "ok"})).is_ok());
        let refused = [
            json!({"tool": "issue_label", "issue": 0, "labels": ["bug"]}),
            json!({"tool": "issue_label", "issue": -1, "labels": ["bug"]}),
            json!({"tool": "issue_label", "issue": 1, "labels": []}),
            json!({"tool": "issue_label", "issue": 1, "labels": ["x".repeat(MAX_LABEL + 1)]}),
            json!({"tool": "issue_label", "issue": 1, "labels": vec!["bug"; MAX_LABELS + 1]}),
            json!({"tool": "issue_comment", "issue": 1, "body": "  "}),
            json!({"tool": "issue_comment", "issue": 1, "body": "x".repeat(MAX_BODY + 1)}),
            json!({"tool": "issue_close_duplicate", "issue": 5, "duplicate_of": 5}),
            json!({"tool": "sudo", "issue": 1}),
        ];
        for event in &refused {
            assert!(parse_action(event).is_err(), "should be refused: {event}");
        }
    }

    #[test]
    fn each_action_is_its_gh_argv_on_the_colonys_own_repository() {
        let args = |a: &Action| action_args("acme/web", a);
        let label = Action::Label {
            issue: 42,
            labels: vec!["bug".into(), "P1".into()],
        };
        assert_eq!(
            args(&label),
            vec![vec![
                "issue",
                "edit",
                "42",
                "-R",
                "acme/web",
                "--add-label",
                "bug",
                "--add-label",
                "P1"
            ]]
        );
        let comment = Action::Comment {
            issue: 42,
            body: "looking".into(),
        };
        assert_eq!(
            args(&comment),
            vec![vec!["issue", "comment", "42", "-R", "acme/web", "--body", "looking"]]
        );
        // Close-duplicate is two commands: the comment that says why, then the close.
        let dup = Action::CloseDuplicate {
            issue: 43,
            duplicate_of: 42,
        };
        assert_eq!(
            args(&dup),
            vec![
                vec!["issue", "comment", "43", "-R", "acme/web", "--body", "Duplicate of #42"],
                vec!["issue", "close", "43", "-R", "acme/web", "--reason", "not planned"],
            ]
        );
    }

    #[test]
    fn a_url_carries_zulu_timestamps_and_encoded_segments() {
        let t = DateTime::parse_from_rfc3339("2026-09-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(stamp(t), "2026-09-24T00:00:00Z", "never the +00:00 offset");
        assert_eq!(encode("release/2.0"), "release%2F2.0");
        assert_eq!(
            encode("repo:acme/web merged:>=2026-09-24"),
            "repo%3Aacme%2Fweb%20merged%3A%3E%3D2026-09-24"
        );
    }

    #[test]
    fn the_ledger_counts_lines_and_reaches_the_cap() {
        let dir = std::env::temp_dir().join(format!("colonizer-github-count-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join(LEDGER);
        assert_eq!(count(&record), 0, "no ledger yet");
        assert!(!spent_cap(&record));
        std::fs::write(&record, "{\"tool\":\"issue_comment\"}\n\n".repeat(MAX_ACTIONS)).unwrap();
        assert_eq!(count(&record), MAX_ACTIONS);
        assert!(spent_cap(&record), "the cap is reached, not exceeded");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn log_of(app: &Shared) -> String {
        std::fs::read_to_string(app.session_dir("abc").join("harness.jsonl")).unwrap()
    }

    /// One `github_action`, from a colony in various states; `perform` runs on it and the caller
    /// reads the outcome back off the ledger and the log.
    async fn act(app: &Shared, id: &str) {
        let rt = app.runtime(id).await;
        perform(
            app.clone(),
            id.into(),
            rt,
            json!({"type": "github_action", "tool": "issue_comment", "issue": 1, "body": "hi"}),
        )
        .await;
    }

    /// A write from a session that is not a GitHub loop, or while the write kill-switch is on, is
    /// refused before `gh`: nothing on the ledger, one log line saying why.
    #[tokio::test]
    async fn a_write_from_the_wrong_session_or_with_writes_blocked_never_reaches_github() {
        let (app, _root) = app_with_colony("abc", SessionStatus::Idle).await;
        act(&app, "abc").await;
        assert!(!app.session_dir("abc").join(LEDGER).exists(), "nothing was recorded");
        assert!(log_of(&app).contains("not a run of a GitHub loop"), "{}", log_of(&app));

        let (app, _root) = github_colony("abc").await;
        let _blocked = crate::authority::test_block_external_writes();
        act(&app, "abc").await;
        assert!(!app.session_dir("abc").join(LEDGER).exists(), "nothing was recorded");
        assert!(log_of(&app).contains("external writes are blocked"), "{}", log_of(&app));
    }

    #[tokio::test]
    async fn the_cap_refuses_a_write_without_touching_github() {
        let (app, _root) = github_colony("abc").await;
        std::fs::write(
            app.session_dir("abc").join(LEDGER),
            "{\"tool\":\"issue_comment\",\"issue\":1}\n".repeat(MAX_ACTIONS),
        )
        .unwrap();
        act(&app, "abc").await;
        assert!(log_of(&app).contains("the most one colony may"), "{}", log_of(&app));
        assert_eq!(
            count(&app.session_dir("abc").join(LEDGER)),
            MAX_ACTIONS,
            "the ledger did not grow"
        );
    }
}
