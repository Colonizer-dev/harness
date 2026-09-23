//! The live link to a colony's agent: one websocket to agentd per session, reconnecting with the
//! last seen `seq`, turning agent events into session state, autopilot decisions, memory proposals
//! and findings.
//!
//! The autopilot decision itself is a pure function (`autopilot_step`) so the policy can be tested
//! apart from the stream it acts on.

use crate::{Shared, findings, github, memory, orgs, provider_quota, spend, util::append_line};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self};

use crate::protocol::{AgentEvent, AgentState};
#[allow(unused_imports)]
use crate::{lifecycle::*, publish::*, queue::*, sessions::*};

#[derive(Debug, PartialEq)]
enum Autopilot {
    Publish,
    Wait(&'static str),
    /// Flags the colony for the maintainer.
    Hold(&'static str),
}

/// The attention reason set when the agent runner never started: the colony stays live (Idle)
/// but has no agent behind it, so it must read as held — the autopilot_held pattern — and never
/// as idle/None.
pub(crate) const AGENT_FAILED: &str = "agent_failed";

/// The marker agentd's spawn-failure status detail carries (colonizer-agentd runner.rs).
const RUNNER_START_FAILURE: &str = "cannot start agent runner";

/// The attention blob for a runner that never started, or nothing for any other failure. Takes the
/// already-built `Session.error` string so the test pins the real mapping; pure so it needs no app
/// state.
fn runner_start_failure_attention(error: Option<&str>) -> Option<Value> {
    error
        .filter(|e| e.contains(RUNNER_START_FAILURE))
        .map(|_| json!({"reason": AGENT_FAILED, "since": Utc::now(), "nudges": 0}))
}

/// What autopilot does when a turn ends; writing `pr.md` during the turn is the agent's signal that it's done.
fn autopilot_step(errored: bool, interrupted: bool, open_question: bool, pr_written: bool) -> Autopilot {
    if open_question {
        Autopilot::Wait("a question is open")
    } else if interrupted {
        Autopilot::Wait("the turn was interrupted")
    } else if errored {
        Autopilot::Hold("the agent's turn ended with an error")
    } else if !pr_written {
        Autopilot::Wait("the agent didn't write or update its PR description this turn")
    } else {
        Autopilot::Publish
    }
}

pub(crate) async fn start_link(app: &Shared, id: &str) {
    let rt = app.runtime(id).await;
    // Before `agent_link` reads `agent_seq` for the reconnect URL: a load that failed to read the
    // stored events must be on the record before the colony starts using the restarted cursor.
    app.report_load_error(id, &rt).await;
    rt.stop.send_replace(false);
    let Some(commands) = rt.commands_rx.lock().await.take() else {
        return;
    };
    tokio::spawn(agent_link(app.clone(), id.to_string(), rt, commands));
}

/// Stays connected to agentd's event stream while the session is live, reconnecting with `since`.
pub(crate) async fn agent_link(app: Shared, id: String, rt: Arc<Runtime>, mut commands: mpsc::UnboundedReceiver<Value>) {
    let mut stop = rt.stop.subscribe();
    let mut backoff = Duration::from_secs(1);
    let mut connected_before = false;
    let mut warned = false;
    loop {
        if *stop.borrow() {
            return;
        }
        let Some(s) = app.session(&id).await else { return };
        if !s.status.is_live() {
            return;
        }
        let since = rt.agent_seq.load(Ordering::SeqCst);
        match agentd_ws(&app, &s, &format!("/v1/events?since={since}")).await {
            Ok(ws) => {
                let message = if connected_before {
                    "reconnected to the agent"
                } else {
                    "connected to the agent"
                };
                app.session_log(&id, "info", message.into()).await;
                connected_before = true;
                warned = false;
                backoff = Duration::from_secs(1);
                if s.status == SessionStatus::Starting {
                    app.update_session(&id, |x| x.status = SessionStatus::Running).await;
                }
                let (mut sink, mut stream) = ws.split();
                loop {
                    tokio::select! {
                        frame = stream.next() => match frame {
                            Some(Ok(tungstenite::Message::Text(text))) => handle_agent_event(&app, &id, &rt, text.as_str()).await,
                            Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => break,
                            Some(Ok(_)) => {}
                        },
                        command = commands.recv() => match command {
                            Some(command) => {
                                if sink.send(tungstenite::Message::Text(command.to_string().into())).await.is_err() {
                                    break;
                                }
                            }
                            None => return,
                        },
                        _ = stop.changed() => {
                            let _ = sink.close().await;
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                if backoff >= Duration::from_secs(8) && !warned {
                    app.session_log(&id, "error", format!("can't reach the agent, retrying: {e:#}"))
                        .await;
                    warned = true;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = stop.changed() => return,
        }
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

pub(crate) async fn handle_agent_event(app: &Shared, id: &str, rt: &Arc<Runtime>, line: &str) {
    let Ok(mut event) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let Some(seq) = event["seq"].as_u64() else { return };
    let (persisted, file_seq, file_line) = {
        let _guard = rt.file_lock.lock().await;
        if seq <= rt.agent_seq.load(Ordering::SeqCst) {
            return; // replayed by agentd after a reconnect: already on record
        }
        // The file's seqs are one counter, agentd lines and host chain lines together, so a browser
        // reconnecting with `?since=` replays one monotonic file. A host chain event has already
        // stamped a rank at or past this agentd event's own seq (validation.rs `emit_chain` numbers
        // from the same file cursor), so the line that would collide or regress the file is
        // renumbered to one past the highest line in it, and remembers its agentd seq in `a_seq`:
        // the reconnect cursor stays agentd's (`agent_seq`), and a host restart reads `a_seq` back
        // exactly (sessions.rs `Runtime::load`). Agentd's own seq is never consumed or advanced by a
        // host event, so the next real agentd event cannot be mistaken for a replay. A line that
        // does not collide is appended byte-for-byte as the runner wrote it.
        let (file_seq, file_line);
        let err = if seq <= rt.last_seq.load(Ordering::SeqCst) {
            event["seq"] = json!(rt.last_seq.load(Ordering::SeqCst) + 1);
            event["a_seq"] = json!(seq);
            file_seq = event["seq"].as_u64().unwrap_or(seq);
            file_line = event.to_string();
            append_line(&rt.events_path, &file_line).await.err()
        } else {
            file_seq = seq;
            file_line = line.to_string();
            append_line(&rt.events_path, line).await.err()
        };
        (
            match err {
                None => {
                    rt.agent_seq.store(seq, Ordering::SeqCst);
                    rt.last_seq.store(file_seq, Ordering::SeqCst);
                    None
                }
                Some(e) => Some(e),
            },
            file_seq,
            file_line,
        )
    };
    if let Some(e) = persisted {
        // The event still reaches every browser below, but the evidence on disk now has a gap, and
        // a gap in the event log must not be silent. The agentd cursor stays put, so if the
        // reconnect's re-fetch of this seq arrives before anything else is appended, the append gets
        // another chance — but once a later event succeeds, `agent_seq` jumps past the lost one and
        // the gap is permanent. This is a second chance, not a retry that is guaranteed to happen.
        app.storage_failed("append to the colony's event log", &e).await;
        app.session_log(
            id,
            "error",
            format!("could not append to {}: {e:#}", rt.events_path.display()),
        )
        .await;
    }
    rt.broadcast(Some(file_seq), file_line.to_string());

    // What the harness acts on is a type, not a bag of fields (docs/agent-events.schema.json). A line
    // outside the contract — a newer runner's event type, or a known one whose body is broken — lands
    // on `Other` and triggers nothing; it has already been forwarded to the browser above, which is
    // the only consumer of most event types anyway.
    let deserialised = serde_json::from_str::<AgentEvent>(&file_line);
    if let Err(e) = &deserialised
        && AgentEvent::is_acted_on(event["type"].as_str().unwrap_or_default())
    {
        // A type this build acts on, whose body it could not read: worth saying out loud, because the
        // runner and the harness have drifted apart on a contract both are supposed to keep.
        app.session_log(
            id,
            "warn",
            format!("ignored a malformed {} event: {e}", event["type"].as_str().unwrap_or("?")),
        )
        .await;
    }

    // Progress for the watchdog: anything but status changes and the echo of its own nudges.
    let watchdog_echo =
        matches!(&deserialised, Ok(AgentEvent::UserMessage { id: echoed, .. }) if echoed.starts_with("watchdog-"));
    if !matches!(&deserialised, Ok(AgentEvent::Status { .. })) && !watchdog_echo {
        {
            let mut activity = rt.activity.lock().await;
            activity.last = Utc::now();
            activity.nudges = 0;
            activity.last_nudge = None;
        }
        if app.session(id).await.is_some_and(|s| s.attention.is_some()) {
            app.update_session(id, |x| x.attention = None).await;
        }
    }

    match deserialised.unwrap_or(AgentEvent::Other) {
        AgentEvent::Status { state, detail } => {
            let next = match state {
                AgentState::Working => SessionStatus::Running,
                AgentState::WaitingForAnswer => SessionStatus::WaitingForAnswer,
                AgentState::Idle | AgentState::Error | AgentState::Exited => SessionStatus::Idle,
                // A state only a newer runner knows: no news this build can act on.
                AgentState::Unknown => return,
            };
            let error = match state {
                AgentState::Error | AgentState::Exited => Some(format!(
                    "agent {}{}",
                    state.as_str(),
                    detail.map(|d| format!(": {d}")).unwrap_or_default()
                )),
                _ => None,
            };
            // A runner that never started leaves the colony live-but-agentless: hold it visibly,
            // the way autopilot_held holds a colony, so it never reads as idle/None. Status
            // events never clear attention (only non-status progress does), so this survives.
            let attention = match state {
                AgentState::Error | AgentState::Exited => runner_start_failure_attention(error.as_deref()),
                _ => None,
            };
            if let Some(current) = app.session(id).await
                && current.status.is_live()
                && (current.status != next || error.is_some())
            {
                app.update_session(id, |x| {
                    x.status = next;
                    if error.is_some() {
                        x.error = error;
                    }
                    if attention.is_some() {
                        x.attention = attention;
                    }
                })
                .await;
            }
        }
        AgentEvent::Question {
            question_id, questions, ..
        } => {
            // The questions travel with the id: autonomous mode answers among the options the
            // agent offered, and nothing else (docs/protocol.md §6.2b).
            *rt.open_question.lock().await = Some((question_id, questions));
            rt.activity.lock().await.question_since = Some(Utc::now());
        }
        AgentEvent::QuestionAnswered { .. } => {
            *rt.open_question.lock().await = None;
            let mut activity = rt.activity.lock().await;
            activity.question_since = None;
            // The question is resolved either way, so an unanswered-provider streak behind it is over.
            activity.judge_failures = 0;
        }
        AgentEvent::MemoryProposal {
            scope,
            title,
            content,
            tags,
        } => {
            memory_proposal(app, id, scope.as_deref(), &title, &content, &tags).await;
        }
        // Spawned: filing talks to GitHub, and the colony's event stream should not wait on it.
        AgentEvent::Finding { .. } => {
            tokio::spawn(file_finding(app.clone(), id.to_string(), rt.clone(), event.clone()));
        }
        AgentEvent::TurnEnd {
            is_error,
            result,
            cost_usd,
            model_usage,
            ..
        } => {
            let cost = cost_usd;
            let usage = model_usage.filter(|u| u.is_object());
            // The spend journal is fed increments, not the cumulative the record is about to carry:
            // appending cumulatives would re-add every earlier turn on the next one. So the record's
            // values are captured before the overwrite and the difference is what gets filed.
            let (old_cost, old_usage) = app
                .session(id)
                .await
                .map(|s| (s.cost_usd, s.model_usage))
                .unwrap_or((None, None));
            if let Some((s, ())) = app
                .update_session(id, |x| {
                    if cost.is_some() {
                        x.cost_usd = cost;
                    }
                    if usage.is_some() {
                        x.model_usage = usage;
                    }
                })
                .await
            {
                spend::record_turn_usage(app, &s.org, old_cost, old_usage.as_ref(), cost, s.model_usage.as_ref()).await;
                // Claude's own cost just landed, so the budget can trip here exactly as it can in the
                // gateway; checked before autopilot, which must not publish a colony the budget stopped.
                enforce_budget(app, id).await;
                let s = app.session(id).await.unwrap_or(s);
                let mark = github::pr_description_mark(&app.session_dir(id).join("out"));
                let pr_written = {
                    let mut last = rt.pr_mark.lock().await;
                    let written = mark.is_some() && *last != mark;
                    *last = mark;
                    written
                };
                let interrupted = rt.interrupted.swap(false, Ordering::SeqCst);
                if s.autopilot && s.status.is_live() {
                    let errored = is_error;
                    let open_question = rt.open_question.lock().await.is_some();
                    match autopilot_step(errored, interrupted, open_question, pr_written) {
                        Autopilot::Publish => {
                            app.session_log(
                                id,
                                "info",
                                "autopilot: the agent finished and wrote its PR description, publishing".into(),
                            )
                            .await;
                            tokio::spawn(publish_session(app.clone(), id.to_string()));
                        }
                        Autopilot::Wait(reason) => {
                            app.session_log(id, "info", format!("autopilot: not publishing yet, {reason}"))
                                .await
                        }
                        Autopilot::Hold(reason) => {
                            app.session_log(
                                id,
                                "warn",
                                format!("autopilot: not publishing, {reason}; press Create PR when the work is ready"),
                            )
                            .await;
                            app.update_session(id, |x| {
                                x.attention = Some(json!({"reason": "autopilot_held", "since": Utc::now(), "nudges": 0}));
                            })
                            .await;
                        }
                    }
                }
            }
            // A turn that died on an empty plan parks the colony instead of holding it: the error
            // text is the only copy of the provider's answer the colony side ever sees.
            if is_error
                && let Some(text) = result.as_deref()
                && let Some(hit) = provider_quota::classify_quota_exhaustion(0, "", text)
            {
                park_quota_colony(app, id, text, &hit).await;
            }
        }
        // Forwarded to the browser above and acted on nowhere here.
        AgentEvent::UserMessage { .. } | AgentEvent::Other => {}
    }
}

/// Parks a colony whose turn died on an exhausted provider: the same stop the budget path takes —
/// microVM removed, worktree kept, slot released — with the quota attention reason instead of a
/// hold. `Stopped` stands in until #213 adds `Parked`; the reason string is the #230 contract.
async fn park_quota_colony(app: &Shared, id: &str, text: &str, hit: &provider_quota::QuotaExhaustion) {
    let providers = app.providers();
    let ids: Vec<String> = providers.iter().map(|p| p.id.clone()).collect();
    let names: Vec<String> = providers.iter().map(|p| p.name.clone()).collect();
    let provider = provider_quota::mentioned_provider(text, &ids, &names);
    if let Some(pid) = &provider {
        app.gateway.mark_quota_exhausted(pid, hit.reset_at.clone(), hit.reset_unix);
    }
    let Some(s) = app.session(id).await else { return };
    if !s.status.is_live() {
        return;
    }
    let error = match (&provider, &hit.reset_at) {
        (Some(pid), Some(reset)) => format!("provider quota exhausted ({pid}, resets {reset})"),
        (Some(pid), None) => format!("provider quota exhausted ({pid})"),
        (None, Some(reset)) => format!("provider quota exhausted (resets {reset})"),
        (None, None) => "provider quota exhausted".to_string(),
    };
    let parked = stop_colony(
        app,
        &s,
        |_| true,
        error,
        "provider quota exhausted; the microVM is removed and the worktree kept, so the colony resumes when the plan refills"
            .into(),
    )
    .await;
    if parked {
        app.update_session(id, |x| {
            x.attention = Some(json!({"reason": provider_quota::QUOTA_EXHAUSTED_REASON, "since": Utc::now(), "nudges": 0}));
        })
        .await;
    }
}

/// A colony proposed a shared-memory note: queue it for review (or, for a repo note with review off,
/// store it marked unreviewed).
pub(crate) async fn memory_proposal(app: &Shared, id: &str, scope: Option<&str>, title: &str, content: &str, tags: &[String]) {
    let Some(s) = app.session(id).await else { return };
    let modules = app.modules.read().await.clone();
    if !orgs::effective_memory_enabled(&modules, &app.org_settings(&s.org)) {
        app.session_log(
            id,
            "info",
            "ignored a memory proposal: shared memory is off for this org".into(),
        )
        .await;
        return;
    }
    let scope = scope.unwrap_or("repo");
    let key = match scope {
        "org" => s.org.clone(),
        "repo" => s.repo.clone(),
        _ => String::new(),
    };
    let source = json!({"session_id": s.id, "repo": s.repo});
    let note = match memory::draft(scope, &key, title, content, tags, source) {
        Ok(note) => note,
        Err(e) => {
            app.session_log(id, "error", format!("rejected a memory proposal: {e:#}"))
                .await;
            return;
        }
    };
    let (title, scope) = (note.title.clone(), note.scope.clone());
    let review = orgs::memory_requires_review(&modules);
    // Only a repo note can skip review. An org or global note reaches every colony in the org or the
    // fleet, so one prompt-injected colony could plant instructions for all of them.
    let stored = if review || scope != "repo" {
        app.memory.add_proposal(note).await.map(|proposal| json!(proposal))
    } else {
        // Only the stored copy says it skipped review: the fallback below queues `note`, and a
        // person approving that is the review.
        let mut unreviewed = note.clone();
        unreviewed.source["reviewed"] = json!(false);
        match memory::store_note(app, unreviewed).await {
            Ok(note) => {
                let mut value = json!(note);
                value["status"] = json!("approved");
                Ok(value)
            }
            // With review off there is no queue to fall back on, so make one: a store that is down
            // (mem0 unreachable, a rejected key) must not cost the colony its proposal.
            Err(e) => {
                app.session_log(
                    id,
                    "warn",
                    format!("could not store the note ({e:#}); queued it for review instead"),
                )
                .await;
                app.memory.add_proposal(note).await.map(|proposal| json!(proposal))
            }
        }
    };
    match stored {
        Ok(proposal) => {
            let waiting = proposal["status"] == "pending";
            let message = format!(
                "memory: the agent proposed \"{title}\" for {scope} memory{}",
                if !waiting {
                    " (review is off, so it is live)"
                } else if !review && scope != "repo" {
                    ", waiting for your review (org and global notes are always reviewed)"
                } else {
                    ", waiting for your review"
                }
            );
            app.session_log(id, "info", message).await;
            let rt = app.runtimes.lock().await.get(id).cloned();
            if let Some(rt) = rt {
                rt.broadcast(None, json!({"type": "memory_proposed", "proposal": proposal}).to_string());
            }
        }
        Err(e) => {
            app.session_log(id, "error", format!("could not store a memory proposal: {e:#}"))
                .await
        }
    }
}

/// A colony's orchestrator confirmed something outside its task. Nothing is filed directly anymore:
/// a host-side validation call judges the finding first, every stage is minuted on the findings
/// ledger and the colony's event stream, and only a validated finding reaches GitHub's cap and
/// issue check. None of it reaches the agent; none of it blocks the event stream — this runs on a
/// spawned task, and the model call that validates is intentionally outside the findings lock, so a
/// slow model cannot hold up another finding's cap check.
pub(crate) async fn file_finding(app: Shared, id: String, rt: Arc<Runtime>, event: Value) {
    let Some(s) = app.session(&id).await else { return };
    let modules = app.modules.read().await.clone();
    if !findings_enabled(&app, &modules) {
        app.session_log(
            &id,
            "info",
            "ignored a finding: filing findings is switched off in Settings".into(),
        )
        .await;
        return;
    }
    let finding = match findings::parse(&event) {
        Ok(finding) => finding,
        Err(e) => {
            app.session_log(&id, "warn", format!("did not file a finding: {e:#}")).await;
            return;
        }
    };
    let decision = match crate::validation::validate(&app, &s, &finding).await {
        Ok(decision) => decision,
        Err(e) => {
            let reason = format!("{e:#}");
            app.session_log(&id, "warn", format!("did not file \"{}\": {reason}", finding.title))
                .await;
            crate::validation::record(
                &app,
                &id,
                &json!({"title": finding.title, "state": "error", "reason": reason}),
            )
            .await;
            return;
        }
    };
    // A rejected finding is fully decided: the ledger and the wire both say so, and nothing further
    // happens — no cap check, no GitHub call, no issue.
    if !decision.real {
        let message = format!("did not file \"{}\": {}", finding.title, decision.reason);
        app.session_log(&id, "info", message).await;
        crate::validation::emit_chain(
            &app,
            &id,
            json!({"type": "rejected", "title": finding.title, "reason": decision.reason}),
        )
        .await;
        crate::validation::record(
            &app,
            &id,
            &json!({"title": finding.title, "state": "rejected", "reason": decision.reason}),
        )
        .await;
        return;
    }
    crate::validation::emit_chain(
        &app,
        &id,
        json!({"type": "validated", "title": finding.title, "severity": decision.severity}),
    )
    .await;
    crate::validation::record(
        &app,
        &id,
        &json!({"title": finding.title, "state": "validated", "severity": decision.severity}),
    )
    .await;
    let _serial = rt.findings_lock.lock().await;
    let dir = app.session_dir(&id);
    let record = dir.join("findings.jsonl");
    if findings::count(&record) >= findings::MAX_PER_COLONY {
        let message = format!(
            "did not file \"{}\": this colony has already filed {} findings, the most one colony may",
            finding.title,
            findings::MAX_PER_COLONY
        );
        app.session_log(&id, "warn", message).await;
        return;
    }
    let outcome = findings::file(&app, &s, &finding, &dir.join("finding-body.md")).await;
    let (level, message, entry) = match &outcome {
        Ok(findings::Filed::Issue(url)) => (
            "info",
            format!("filed finding \"{}\" as {url}", finding.title),
            json!({"title": finding.title, "state": "filed", "issue": url}),
        ),
        Ok(findings::Filed::Duplicate(url)) => (
            "info",
            format!("did not file \"{}\": {url} is already open with that title", finding.title),
            json!({"title": finding.title, "state": "duplicate", "duplicate_of": url}),
        ),
        Err(e) => (
            "error",
            format!("could not file finding \"{}\": {e:#}", finding.title),
            Value::Null,
        ),
    };
    // Only a filed or matched finding counts toward the cap, so the line that carries the issue or
    // its duplicate is the one appended under the lock; a GitHub error should not use one up.
    if !entry.is_null() {
        let recorded = append_line(&record, &entry.to_string()).await;
        if let Err(e) = recorded {
            // The finding was still filed on GitHub (that happened above); what failed is the
            // colony's own record of it, so say so instead of letting the gap pass silently.
            app.storage_failed("append to the colony's findings log", &e).await;
            app.session_log(
                &id,
                "error",
                format!("could not record the finding in {}: {e:#}", record.display()),
            )
            .await;
        }
    }
    app.session_log(&id, level, message).await;
    if let Ok(findings::Filed::Issue(url)) = &outcome
        && crate::sessions::autofix_enabled(&app, &s).await
    {
        // The fix colony starts off this path: filing has already returned, the issue is on GitHub,
        // and the colony's event stream must not wait for a second colony to boot.
        let title = finding.title.clone();
        let hunting = s.id.clone();
        let fix_colony = crate::validation::spawn_fix_colony(app.clone(), s.clone(), finding.clone(), url.clone());
        tokio::spawn(async move {
            if let Err(e) = fix_colony.await {
                let reason = format!("{e:#}");
                app.session_log(
                    &hunting,
                    "warn",
                    format!("could not start the fix colony for \"{title}\": {reason}"),
                )
                .await;
                crate::validation::record(&app, &hunting, &json!({"title": title, "state": "error", "reason": reason})).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autopilot_publishes_only_a_clean_turn_that_wrote_the_pr_description() {
        // (errored, interrupted, open_question, pr_written)
        assert_eq!(autopilot_step(false, false, false, true), Autopilot::Publish);
        assert!(matches!(autopilot_step(false, false, false, false), Autopilot::Wait(_)));
        assert!(matches!(autopilot_step(false, false, true, true), Autopilot::Wait(_)));
        assert!(matches!(autopilot_step(true, true, false, true), Autopilot::Wait(_)));
        assert!(matches!(autopilot_step(true, false, false, true), Autopilot::Hold(_)));
        assert!(matches!(autopilot_step(true, false, false, false), Autopilot::Hold(_)));
    }

    #[test]
    fn runner_start_failure_holds_the_colony_visibly() {
        // The handler's exact mapping at the pure level: the error the Error/Exited arm builds,
        // then the attention derived from it.
        let detail = "cannot start agent runner `node`: No such file or directory (os error 2)";
        let error = Some(format!("agent {}: {detail}", AgentState::Error.as_str()));
        assert!(error.is_some());
        let attention = runner_start_failure_attention(error.as_deref());
        assert_eq!(
            attention.as_ref().and_then(|a| a["reason"].as_str()),
            Some("agent_failed"),
            "a spawn failure must name its attention reason, never idle/None"
        );
        // Any other failure is error-only: no attention.
        let exited = Some(format!("agent {}: exit code 1", AgentState::Exited.as_str()));
        assert!(runner_start_failure_attention(exited.as_deref()).is_none());
        assert!(runner_start_failure_attention(None).is_none());
    }

    /// Review off lets a colony's repo note straight through, marked unreviewed. An org or global
    /// note reaches every colony in the org or the fleet, so it waits for a person whatever the setting.
    #[tokio::test]
    async fn with_review_off_only_a_repo_note_skips_the_queue() {
        async fn set(app: &Shared, key: &str, value: Value) {
            app.modules.write().await.memory.settings.insert(key.into(), value);
        }
        let (app, root) = crate::sessions::tests::app_with_colony("abc", SessionStatus::Running).await;
        set(&app, "require_review", json!(false)).await;
        for (scope, key) in [("org", "acme"), ("global", "")] {
            memory_proposal(&app, "abc", Some(scope), "Sign commits", "Always sign.", &[]).await;
            assert!(app.memory.notes(scope, key).await.unwrap().is_empty(), "{scope}");
        }
        assert_eq!(app.memory.proposals().await.len(), 2);

        memory_proposal(&app, "abc", None, "Run tests locked", "Use --locked.", &[]).await;
        let notes = app.memory.notes("repo", "acme/repo").await.unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].source["reviewed"], json!(false));
        assert_eq!(app.memory.proposals().await.len(), 2, "the repo note did not queue");

        // A repo note the store cannot take is queued instead, unmarked: approving it is its review.
        app.modules.write().await.memory.provider = memory::MEM0.into();
        set(&app, "base_url", json!("ftp://nowhere")).await;
        memory_proposal(&app, "abc", None, "Deploys", "Stage first.", &[]).await;
        let pending = app.memory.proposals().await;
        assert_eq!(pending.len(), 3);
        assert!(pending.iter().all(|p| p.note.source["reviewed"].is_null()), "{pending:?}");

        app.modules.write().await.memory.provider = "files".into();
        set(&app, "require_review", json!(true)).await;
        memory_proposal(&app, "abc", Some("repo"), "Commit style", "Keep commits small.", &[]).await;
        assert_eq!(app.memory.notes("repo", "acme/repo").await.unwrap().len(), 1);
        assert_eq!(app.memory.proposals().await.len(), 4);
        let _ = std::fs::remove_dir_all(root);
    }
}
