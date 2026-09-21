//! Host-side validation of a colony's findings, and the autofix pipeline that follows them.
//!
//! A colony's orchestrator has already confirmed a finding before it is sent; this module is the
//! second, host-side check. Every finding is judged by a one-shot call to the orchestrator model
//! — the same provider route the autonomous judge uses (autonomy.rs) — and only a finding that
//! passes is filed. The outcome of every stage, from validation through a fix colony's merge, is
//! recorded on the hunter's findings ledger and on its event stream, so nothing happens to a
//! repository that is not minuted first.
//!
//! Autofix is the sharpest edge here: a filed finding spawns a *second* colony whose change is
//! reviewed by a fresh independent session before it can merge, and it merges only on a passing
//! review *and* an explicit opt-in. The review never reuses the author's transcript — a brand-new
//! conversation gets the diff and the finding, never the author's history.

use crate::{
    App, Shared,
    config::{ModulesConfig, setting_str},
    findings::Finding,
    modules::schema_for,
    sessions::{FixFor, NewSession, Session, automerge_enabled},
    util::{append_line, short_id, truncate},
};
use anyhow::{Context, Result, bail};
use axum::{Json, extract::State};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

/// How much of a fix colony's diff the reviewer is asked to judge. A fix colony is told to keep the
/// change small, so a diff that still overruns this bound is itself a sign it did not.
const DIFF_BUDGET: usize = 100_000;

/// What one validation call decided, held so the caller can minute it before filing anything.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Decision {
    pub real: bool,
    pub severity: String,
    pub reason: String,
}

/// Reads the validator's JSON out of its reply, tolerating prose around it. A missing or unknown
/// severity reads as the middle of the range — never a level the ledger cannot compare against —
/// and a rejection with no reason is not a decision at all, so it is refused like a reply that is
/// not JSON.
pub(crate) fn parse_decision(text: &str) -> Option<Decision> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let parsed: Value = serde_json::from_str(text.get(start..=end)?).ok()?;
    let real = parsed["real"].as_bool()?;
    let severity = match parsed["severity"].as_str() {
        Some(level @ ("low" | "medium" | "high" | "critical")) => level.to_string(),
        _ => "medium".to_string(),
    };
    let reason = parsed["reason"].as_str().unwrap_or_default().trim().to_string();
    if !real && reason.is_empty() {
        return None;
    }
    Some(Decision { real, severity, reason })
}

/// What one independent review decided: whether the fix passes, and a review body worth commenting.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Verdict {
    pub vote: String,
    pub body: String,
}

/// Reads the reviewer's JSON out of its reply, exactly as the validator's is read; the vote must be
/// a pass or a fail, and a review that says nothing is not a review.
pub(crate) fn parse_verdict(text: &str) -> Option<Verdict> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let parsed: Value = serde_json::from_str(text.get(start..=end)?).ok()?;
    let vote = match parsed["verdict"].as_str() {
        Some(vote @ ("pass" | "fail")) => vote.to_string(),
        _ => return None,
    };
    let body = parsed["body"].as_str().unwrap_or_default().trim().to_string();
    if body.is_empty() {
        return None;
    }
    Some(Verdict { vote, body })
}

/// The hard invariant of autofix: the reviewing conversation must be a fresh, independent session,
/// never the colony that wrote the change. Called before any of the review's work, so a session
/// that would review its own pull request is refused the moment it is named.
pub(crate) fn ensure_independent(author: &str, reviewer: &str) -> Result<()> {
    if author == reviewer {
        bail!("reviewer session must be independent of the author colony");
    }
    Ok(())
}

/// What the validator is asked: the repository plus the agent's whole claim, and a reply contract
/// that is the only thing the host will read. The finding's fields arrive bounded (findings.rs), so
/// the prompt cannot be forced unbounded.
fn validation_prompt(repo: &str, finding: &Finding) -> String {
    format!(
        "A coding agent working on {repo} has reported something it noticed outside its own task. \
         Decide whether it is a real, actionable bug worth filing as a GitHub issue on {repo}: \
         reproducible and specific beats vague, and a real bug the agent can point at beats an \
         elegant worry.\n\n\
         ## Title\n\n{}\n\n## What the agent saw\n\n{}\n\n## How the agent confirmed it\n\n{}\n\n\
         Reply with JSON only, no prose around it:\n\n\
         {{\"real\": true or false, \"severity\": \"low\" or \"medium\" or \"high\" or \"critical\", \
         \"reason\": \"one sentence\"}}",
        finding.title, finding.body, finding.evidence
    )
}

/// What the independent reviewer is asked: the finding's own filing and the change's diff, nothing
/// more — no previous turns, no author context, so the judgement cannot be pre-steered.
fn review_prompt(repo: &str, issue_url: &str, title: &str, diff: &str) -> String {
    format!(
        "A coding agent opened a pull request on {repo} to fix the filed finding at {issue_url}, \
         \"{title}\". Judge the change on its own against the kind of problem the title names.\n\n\
         ## The diff\n\n{diff}\n\n\
         Review this change as an independent reviewer: is it a correct, complete, minimal fix, with \
         nothing in it beyond the finding? Reply with JSON only, no prose around it:\n\n\
         {{\"verdict\": \"pass\" or \"fail\", \"body\": \"a short markdown review\"}}"
    )
}

/// The orchestrator model host-side calls run on: the agent module's `model` setting, the same one
/// a colony's own orchestrator would resolve (sessions.rs `boot`). Without one there is nothing to
/// judge with, so the call refuses and the finding is recorded as an error rather than filed
/// unjudged — a model that cannot be reached must never silently mean "file it anyway".
async fn orchestrator_model(app: &App, modules: &ModulesConfig) -> Result<String> {
    let schema = schema_for("agent", &modules.agent.provider, &app.agents);
    let model = setting_str(&modules.agent, &schema, "model").trim().to_string();
    if model.is_empty() {
        bail!(
            "no orchestrator model is configured: set the agent module's model in Settings, and \
             nothing can be validated against the finding"
        );
    }
    Ok(model)
}

/// One validation call: a one-shot orchestrator-model request, exactly the route the autonomous
/// judge uses. The reply is parsed by [`parse_decision`]; a reply that is not the JSON asked for is
/// an error, and an error files nothing.
pub(crate) async fn validate(app: &App, s: &Session, finding: &Finding) -> Result<Decision> {
    let modules = app.modules.read().await.clone();
    let model = orchestrator_model(app, &modules).await?;
    let reply = crate::autonomy::ask_model(app, &model, &validation_prompt(&s.repo, finding)).await?;
    parse_decision(&reply).context("the validator's reply was not the JSON it was asked for")
}

/// The host-generated event types `emit_chain` writes, reserved to the mothership by the protocol
/// (docs/protocol.md §6.6): the runner never emits them, so `sessions.rs` `Runtime::load` cuts them
/// out of the agentd reconnect cursor by their type when it restarts a mid-life colony.
pub(crate) fn is_host_chain_type(kind: &str) -> bool {
    matches!(kind, "validated" | "rejected" | "fix_colony" | "review" | "merged")
}

/// Appends a host-generated event to the colony's event log and broadcasts it on its websocket,
/// exactly the way an agentd event lands (events.rs `handle_agent_event`): appended under the
/// file lock with the next `seq`, then broadcast so an open browser sees it, and persisted so a
/// browser that reconnects replays it from the file. Host-generated only — the line is never sent
/// back to the runner.
pub(crate) async fn emit_chain(app: &Shared, session_id: &str, mut event: Value) {
    let rt = app.runtime(session_id).await;
    if event.get("ts").is_none() {
        // The same frame shape agentd stamps, so the browser renders it identically.
        event["ts"] = json!(Utc::now());
    }
    let (seq, line, persisted) = {
        let _guard = rt.file_lock.lock().await;
        // One file counter for agentd lines and host lines alike, so a reconnecting browser replays
        // one monotonic stream. Stamped under the lock: the file seq and the append must move
        // together, or two host events racing could write their lines out of order. The agentd
        // dedupe cursor (`agent_seq`) does not move here — only agentd events advance it — so a
        // host event can never consume the rank of the next real agent event (events.rs). No caller
        // pre-numbers a host line today, but a given one still must not regress the file, so the
        // rank is whichever is higher out of the given seq and one past the cursor.
        let file_cursor = rt.last_seq.load(Ordering::SeqCst);
        let seq = match event.get("seq").and_then(Value::as_u64) {
            Some(given) => given,
            None => file_cursor + 1,
        }
        .max(file_cursor + 1);
        event["seq"] = json!(seq);
        // Reserved even when the append fails, as before: the next line must stamp past a rank a
        // browser may have just seen broadcast, or that broadcast and a later line could share a seq.
        rt.last_seq.store(seq, Ordering::SeqCst);
        let line = event.to_string();
        let err = append_line(&rt.events_path, &line).await.err();
        (Some(seq), line, err)
    };
    if let Some(e) = persisted {
        // The line still reaches every open browser; what failed is the durable copy, and a gap in
        // the host's own event log must not be silent any more than a gap in agentd's would be.
        app.storage_failed("append to the colony's event log", &e).await;
        app.session_log(
            session_id,
            "error",
            format!("could not append to {}: {e:#}", rt.events_path.display()),
        )
        .await;
    }
    rt.broadcast(seq, line);
}

/// Appends one line to a session's findings ledger. The cap check, GitHub filing and the ledger all
/// travel with the findings lock in `file_finding`, so this side takes the same lock: a validation
/// or autofix outcome landing here must never interleave a cap decision mid-write.
pub(crate) async fn record(app: &Shared, session_id: &str, line: &Value) {
    let rt = app.runtime(session_id).await;
    let path = app.session_dir(session_id).join("findings.jsonl");
    let appended = {
        let _guard = rt.findings_lock.lock().await;
        append_line(&path, &line.to_string()).await
    };
    if let Err(e) = appended {
        // The outcome is real in memory either way; what failed is the colony's record of it, and
        // the gap is reported rather than passed silently.
        app.storage_failed("append to the colony's findings log", &e).await;
        app.session_log(
            session_id,
            "error",
            format!("could not record the outcome in {}: {e:#}", path.display()),
        )
        .await;
    }
}

/// Starts the fix colony a filed finding asked for, on its own task. The finding travels in the
/// instructions with the issue URL; the new colony is told to fix exactly that, keep the change
/// small, and write its pull request description to `/harness/out/pr.md` like any other colony. The
/// link back to the hunter is what the later review reports on.
///
/// This is a plain function returning a boxed future, on purpose: it reaches into session creation,
/// whose own future type reaches back into the event loop, and a cycle in the loop's future type —
/// `create` booting a colony whose event link can file findings that re-create — would stop the
/// crate compiling. The box cuts that dependency: the event loop only ever sees a `Pin<Box<dyn
/// Future>>`, never the machinery inside.
pub(crate) fn spawn_fix_colony(
    app: Shared,
    hunter: Session,
    finding: Finding,
    issue_url: String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>> {
    Box::pin(spawn_fix_colony_inner(app, hunter, finding, issue_url))
}

async fn spawn_fix_colony_inner(app: Shared, hunter: Session, finding: Finding, issue_url: String) -> Result<String> {
    let title = format!("Fix: {}", truncate(&finding.title, 250));
    let instructions = format!(
        "A finding was filed for this repository at {issue_url}: {}\n\n{}\n\nEvidence: {}\n\n\
         Fix ONLY the reported problem, nothing else. Keep the pull request small and reviewable. \
         When you are done, write the pull request description to /harness/out/pr.md.",
        finding.title, finding.body, finding.evidence
    );
    let created = crate::sessions::create(
        State(app.clone()),
        Json(NewSession {
            repo: hunter.repo.clone(),
            issue: None,
            title,
            instructions,
            autopilot: None,
            // A fix colony fixes the finding; it does not spawn fix colonies of its own. Automerge
            // however carries the hunter's decision, so a passing review can still merge.
            autofix: Some(false),
            automerge: Some(automerge_enabled(&app, &hunter).await),
            allow_duplicate: false,
            model_tier: None,
            after: None,
        }),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{:#}", e.1))?;
    let fix = created.0;
    // The link is written before anything else on the fix colony: the review that follows reads it
    // to know whose finding this was and where its outcome belongs.
    app.update_session(&fix.id, |s| {
        s.fix_for = Some(FixFor {
            session: hunter.id.clone(),
            title: finding.title.clone(),
            issue: Some(issue_url.clone()),
        })
    })
    .await;
    emit_chain(
        &app,
        &hunter.id,
        json!({"type": "fix_colony", "title": finding.title, "session": fix.id.clone(), "issue": issue_url.clone()}),
    )
    .await;
    record(
        &app,
        &hunter.id,
        &json!({"title": finding.title, "state": "fix_colony", "fix_session": fix.id.clone(), "issue": issue_url.clone()}),
    )
    .await;
    app.session_log(
        &hunter.id,
        "info",
        format!("spawned fix colony {} for \"{}\" ({issue_url})", fix.id, finding.title),
    )
    .await;
    Ok(fix.id)
}

/// Reviews a fix colony's pull request once it is open, and merges it when the review passes and
/// was opted in. Spawned from the publish path, so it must run without blocking it — and without
/// panicking: every failure lands in the fix colony's log and the hunter's ledger, and the pull
/// request stays open for a person either way.
pub(crate) async fn review_fix_pr(app: Shared, fix_id: String) {
    if let Err(e) = review_fix_pr_inner(&app, &fix_id).await {
        let reason = format!("{e:#}");
        app.session_log(&fix_id, "warn", format!("fix review: {reason}")).await;
        if let Some((title, hunter)) = app
            .session(&fix_id)
            .await
            .and_then(|s| s.fix_for)
            .map(|f| (f.title, f.session))
            && app.session(&hunter).await.is_some()
        {
            record(&app, &hunter, &json!({"title": title, "state": "error", "reason": reason})).await;
        }
    }
}

async fn review_fix_pr_inner(app: &Shared, fix_id: &str) -> Result<()> {
    let fix = app.session(fix_id).await.context("the fix colony is gone")?;
    let fix_for = fix.fix_for.clone().context("this colony was not spawned to fix a finding")?;
    let pr_url = fix.pr_url.clone().context("the fix colony has no pull request to review")?;

    // A fresh id names the reviewing conversation, and it must never be the author's own session:
    // refused before any of the work below, so the self-review the issue worries about cannot start.
    let review_id = short_id();
    ensure_independent(fix_id, &review_id)?;

    let diff = crate::util::exec(&mut app.gh(["pr", "diff", pr_url.as_str()]))
        .await
        .context("could not fetch the pull request's diff")?;
    let modules = app.modules.read().await.clone();
    let model = orchestrator_model(app, &modules).await?;
    let issue_url = fix_for.issue.as_deref().unwrap_or(pr_url.as_str());
    let reply = crate::autonomy::ask_model(
        app,
        &model,
        &review_prompt(&fix.repo, issue_url, &fix_for.title, &truncate(&diff, DIFF_BUDGET)),
    )
    .await?;
    let verdict = parse_verdict(&reply).context("the reviewer's reply was not the JSON it was asked for")?;

    // The review's outcome is minuted on the hunter before anything is decided on it; if the hunter
    // is gone, the fix colony's own log above carries on without it.
    let hunter = fix_for.session.clone();
    if app.session(&hunter).await.is_some() {
        record(
            app,
            &hunter,
            &json!({"title": fix_for.title.clone(), "state": "review", "review_session": review_id, "verdict": verdict.vote, "pr": pr_url}),
        )
        .await;
        emit_chain(
            app,
            &hunter,
            json!({"type": "review", "title": fix_for.title.clone(), "session": review_id, "verdict": verdict.vote, "pr": pr_url}),
        )
        .await;
    }

    if verdict.vote == "pass" && automerge_enabled(app, &fix).await {
        app.session_log(fix_id, "info", format!("the independent review of {pr_url} passed; merging"))
            .await;
        crate::util::exec(&mut app.gh(["pr", "merge", pr_url.as_str(), "--squash"]))
            .await
            .context("the review passed, but the merge failed")?;
        if app.session(&hunter).await.is_some() {
            record(
                app,
                &hunter,
                &json!({"title": fix_for.title.clone(), "state": "merged", "pr": pr_url}),
            )
            .await;
            emit_chain(
                app,
                &hunter,
                json!({"type": "merged", "title": fix_for.title.clone(), "session": fix_id, "pr": pr_url}),
            )
            .await;
        }
        app.session_log(fix_id, "info", format!("merged {pr_url}")).await;
    } else if verdict.vote == "pass" {
        app.session_log(
            fix_id,
            "info",
            format!("the independent review of {pr_url} passed, but automerge is off; the PR is left open for you"),
        )
        .await;
    } else {
        // The review travels to GitHub by file — the body is review-length, not argv-length — and a
        // copy stays in the session directory so the text survives even a posting failure.
        let body_path = app.session_dir(fix_id).join("review.md");
        std::fs::write(&body_path, &verdict.body)?;
        let mut cmd = app.gh(["pr", "comment", pr_url.as_str(), "--body-file"]);
        cmd.arg(&body_path);
        crate::util::exec(&mut cmd)
            .await
            .context("the review was written, but the comment could not be posted")?;
        app.session_log(
            fix_id,
            "info",
            format!("the independent review of {pr_url} did not pass; the change is left open with the review as a comment"),
        )
        .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a passing validation reply licenses, and what each flaw stops.
    #[test]
    fn decision_parsing_is_a_table_of_licences_and_refusals() {
        let decided = parse_decision(
            r#"Sure — here is my judgement:
        {"real": true, "severity": "high", "reason": "the error path drops the lock"}"#,
        )
        .expect("prose around the JSON is tolerated");
        assert!(decided.real);
        assert_eq!(decided.severity, "high");
        assert_eq!(decided.reason, "the error path drops the lock");

        // The default severity is the middle of the range, whether it is missing or unknown.
        assert_eq!(parse_decision(r#"{"real":true}"#).unwrap().severity, "medium");
        assert_eq!(
            parse_decision(r#"{"real":true,"severity":"life-altering"}"#)
                .unwrap()
                .severity,
            "medium"
        );
        assert_eq!(
            parse_decision(r#"{"real":true,"severity":"critical"}"#).unwrap().severity,
            "critical"
        );

        // A rejection must say why; a reply without a real flag is not a decision; neither is prose.
        assert!(parse_decision(r#"{"real":false,"reason":""}"#).is_none());
        assert!(parse_decision(r#"{"real":false}"#).is_none());
        assert!(parse_decision(r#"{"severity":"low","reason":"no"}"#).is_none());
        assert!(parse_decision("it is clearly not a bug").is_none());
        assert!(parse_decision(r#"{"real":false,"reason":"not reproducible"}"#).is_some());
    }

    #[test]
    fn verdict_parsing_accepts_only_a_pass_or_fail_that_says_something() {
        let verdict = parse_verdict(
            r#"Here is my review:
        {"verdict": "fail", "body": "the fix leaves the cache warm"}"#,
        )
        .expect("prose around the JSON is tolerated");
        assert_eq!(verdict.vote, "fail");
        assert_eq!(verdict.body, "the fix leaves the cache warm");
        assert_eq!(
            parse_verdict(r#"{"verdict":"pass","body":"looks right"}"#).unwrap().vote,
            "pass"
        );
        assert!(parse_verdict(r#"{"verdict":"maybe","body":"hmm"}"#).is_none());
        assert!(parse_verdict(r#"{"verdict":"pass"}"#).is_none(), "a review says something");
        assert!(parse_verdict("looks good to me").is_none());
    }

    #[test]
    fn a_session_cannot_review_its_own_pull_request() {
        let error = ensure_independent("abc", "abc").unwrap_err().to_string();
        assert_eq!(error, "reviewer session must be independent of the author colony");
        assert!(ensure_independent("abc", "def").is_ok(), "a fresh session reviews freely");
    }

    #[tokio::test]
    async fn a_host_chain_event_appends_to_the_event_log_and_broadcasts_with_a_seq() {
        use crate::sessions::SessionStatus;
        use crate::sessions::tests::app_with_colony;
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let rt = app.runtime("abc").await;
        let mut live = rt.events.subscribe();
        let received = tokio::spawn(async move { live.recv().await.map(|broadcast| broadcast.json.clone()) });
        emit_chain(&app, "abc", json!({"type": "validated", "title": "career pages"})).await;
        let line = received.await.unwrap().expect("the broadcast arrived");
        let event: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(event["type"], "validated");
        assert_eq!(event["title"], "career pages");
        assert_eq!(event["seq"], 1, "the next seq after nothing is 1");
        assert!(event["ts"].is_string(), "the ts is present for the browser's clock");
        let stored = std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap();
        assert!(
            stored.contains("career pages"),
            "the line is persisted for a reconnecting browser: {stored}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_recorded_outcome_appends_one_ledger_line_under_the_findings_lock() {
        use crate::sessions::SessionStatus;
        use crate::sessions::tests::app_with_colony;
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        record(
            &app,
            "abc",
            &json!({"title": "career pages", "state": "validated", "severity": "high"}),
        )
        .await;
        let ledger = app.session_dir("abc").join("findings.jsonl");
        let stored = std::fs::read_to_string(&ledger).unwrap();
        assert!(stored.contains("\"state\":\"validated\""), "{stored}");
        assert!(stored.ends_with('\n'), "every line ends the file's append format");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The seq-space collision this module used to cause: a host chain event takes the next file
    /// seq, and the very next agentd event — carrying that same seq, because the two came from the
    /// same counter back then — was dropped as a "replay", never persisted and never broadcast. With
    /// the agentd dedupe cursor kept apart from the file cursor it must land: renumbered to keep the
    /// file monotonic, remembering its own seq in `a_seq`, and a host restart must recover both
    /// cursors so the reconnect asks agentd for exactly what it missed.
    #[tokio::test]
    async fn a_host_chain_event_does_not_consume_the_next_agentd_seq_or_drop_that_event() {
        use crate::events::handle_agent_event;
        use crate::sessions::SessionStatus;
        use crate::sessions::tests::app_with_colony;
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        let rt = app.runtime("abc").await;
        let mut live = rt.events.subscribe();

        handle_agent_event(&app, "abc", &rt, r#"{"seq":1,"type":"status","state":"working"}"#).await;
        emit_chain(&app, "abc", json!({"type": "validated", "title": "career pages"})).await;
        // The same seq the host event just stamped: before the two cursors, this was the event the
        // guard threw away.
        handle_agent_event(&app, "abc", &rt, r#"{"seq":2,"type":"status","state":"idle"}"#).await;

        let events = std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap();
        let lines: Vec<Value> = events.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        assert_eq!(lines.len(), 3, "the agentd event is not dropped: {events}");
        let seqs: Vec<u64> = lines.iter().map(|l| l["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, vec![1, 2, 3], "the file stays monotonic for ?since= replay: {events}");
        let kept = &lines[2];
        assert_eq!(kept["type"], "status", "the kept line is the agentd line: {kept}");
        assert_eq!(kept["state"], "idle");
        assert_eq!(
            kept["a_seq"], 2,
            "its own seq survives the renumbering, for the restart: {kept}"
        );
        assert_eq!(
            rt.agent_seq.load(Ordering::SeqCst),
            2,
            "the dedupe cursor is agentd's last own seq, not the host's"
        );
        assert_eq!(
            rt.last_seq.load(Ordering::SeqCst),
            3,
            "the file cursor is the last written seq"
        );

        // All three lines reached an open browser, each exactly once, in order. The status handler
        // also broadcasts `session` frames between them, so the receiver is drained until the three
        // event lines are seen rather than assuming a fixed message count.
        let mut seen = Vec::new();
        for _ in 0..8 {
            let line = live.recv().await.unwrap().json.clone();
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && let Some(seq) = v["seq"].as_u64()
            {
                seen.push((seq, v));
            }
            if seen.len() == 3 {
                break;
            }
        }
        assert_eq!(seen.len(), 3, "the event lines were each broadcast once");
        assert_eq!(seen[0].0, 1);
        assert_eq!(seen[1].0, 2);
        assert_eq!(seen[1].1["type"], "validated");
        assert_eq!(seen[2].0, 3, "broadcast uses the file seq, so replay and live agree");
        assert_eq!(seen[2].1["type"], "status");
        assert_eq!(seen[2].1["state"], "idle");

        // A replayed duplicate of an agentd line is still refused, as a reconnect would send it.
        handle_agent_event(&app, "abc", &rt, r#"{"seq":1,"type":"status","state":"working"}"#).await;
        let again = std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap();
        assert_eq!(again.lines().count(), 3, "a replayed agentd event is not recorded twice");

        // A host restart loads both cursors exactly: the reconnect asks agentd for what is missing.
        app.runtimes.lock().await.remove("abc");
        let rt2 = app.runtime("abc").await;
        assert_eq!(
            rt2.agent_seq.load(Ordering::SeqCst),
            2,
            "the host restart recovers agentd's own seq"
        );
        assert_eq!(rt2.last_seq.load(Ordering::SeqCst), 3, "and the file cursor");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The full autofix start, as `file_finding` would spawn it: a second colony is created on the
    /// hunter's repository, linked back to its finding, and the hunter's ledger and event stream
    /// both tell the story. The fix colony's boot task is spawned but never polled on the test
    /// runtime, so nothing reaches GitHub or a microVM.
    #[tokio::test]
    async fn spawning_a_fix_colony_links_it_to_its_hunter_and_minutes_the_start() {
        use crate::sessions::SessionStatus;
        use crate::sessions::tests::{app_that_can_create, colony};
        let root = std::env::temp_dir().join(format!("colonizer-validation-{}", short_id()));
        let app = app_that_can_create(&root);
        let hunter = {
            let mut h = colony("acme", SessionStatus::Running);
            h.id = "hunter".into();
            h.repo = "acme/app".into();
            h
        };
        tokio::fs::create_dir_all(app.session_dir("hunter")).await.unwrap();
        app.sessions.write().await.push(hunter.clone());
        let finding = Finding {
            title: "career pages are promised".into(),
            body: "llms.txt says the pages exist".into(),
            evidence: "read model.rs".into(),
        };
        let issue_url = "https://github.com/acme/app/issues/9".to_string();
        let fix_id = spawn_fix_colony(app.clone(), hunter, finding, issue_url)
            .await
            .expect("the fix colony can be created");
        let fix = app.session(&fix_id).await.expect("the fix colony is listed");
        assert_eq!(fix.repo, "acme/app");
        assert_eq!(fix.autofix, Some(false), "a fix colony does not cascade further fix colonies");
        let linked = fix.fix_for.as_ref().expect("the fix colony knows its hunter");
        assert_eq!(linked.session, "hunter");
        assert_eq!(linked.issue.as_deref(), Some("https://github.com/acme/app/issues/9"));
        let ledger = std::fs::read_to_string(app.session_dir("hunter").join("findings.jsonl")).unwrap();
        assert!(
            ledger.contains(&format!("\"fix_session\":\"{fix_id}\"")),
            "the hunter's ledger records the colony: {ledger}"
        );
        let events = std::fs::read_to_string(app.session_dir("hunter").join("events.jsonl")).unwrap();
        let event: Value = events.lines().last().and_then(|l| serde_json::from_str(l).ok()).unwrap();
        assert_eq!(event["type"], "fix_colony");
        assert_eq!(event["session"], fix_id);
        assert_eq!(event["issue"], "https://github.com/acme/app/issues/9");
        let _ = std::fs::remove_dir_all(root);
    }
}
