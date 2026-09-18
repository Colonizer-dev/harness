//! The live link to a colony's agent: one websocket to agentd per session, reconnecting with the
//! last seen `seq`, turning agent events into session state, autopilot decisions, memory proposals
//! and findings.
//!
//! The autopilot decision itself is a pure function (`autopilot_step`) so the policy can be tested
//! apart from the stream it acts on.

use crate::{Shared, findings, github, memory, orgs, util::append_line};
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
    // Before `agent_link` reads `last_seq` for the reconnect URL: a load that failed to read the
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
        let since = rt.last_seq.load(Ordering::SeqCst);
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
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let Some(seq) = event["seq"].as_u64() else { return };
    let persisted = {
        let _guard = rt.file_lock.lock().await;
        if seq <= rt.last_seq.load(Ordering::SeqCst) {
            return; // replayed after a reconnect
        }
        match append_line(&rt.events_path, line).await {
            Ok(()) => {
                rt.last_seq.store(seq, Ordering::SeqCst);
                None
            }
            Err(e) => Some(e),
        }
    };
    if let Some(e) = persisted {
        // The event still reaches every browser below, but the evidence on disk now has a gap, and
        // a gap in the event log must not be silent. `last_seq` stays put, so if the reconnect's
        // re-fetch of this seq arrives before anything else is appended, the append gets another
        // chance — but once a later event succeeds, `last_seq` jumps past the lost one and the gap
        // is permanent. This is a second chance, not a retry that is guaranteed to happen.
        app.storage_failed("append to the colony's event log", &e).await;
        app.session_log(
            id,
            "error",
            format!("could not append to {}: {e:#}", rt.events_path.display()),
        )
        .await;
    }
    rt.broadcast(Some(seq), line.to_string());

    // What the harness acts on is a type, not a bag of fields (docs/agent-events.schema.json). A line
    // outside the contract — a newer runner's event type, or a known one whose body is broken — lands
    // on `Other` and triggers nothing; it has already been forwarded to the browser above, which is
    // the only consumer of most event types anyway.
    let deserialised = serde_json::from_str::<AgentEvent>(line);
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
            if let Some(current) = app.session(id).await
                && current.status.is_live()
                && (current.status != next || error.is_some())
            {
                app.update_session(id, |x| {
                    x.status = next;
                    if error.is_some() {
                        x.error = error;
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
            cost_usd,
            model_usage,
            ..
        } => {
            let cost = cost_usd;
            let usage = model_usage.filter(|u| u.is_object());
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
        }
        // Forwarded to the browser above and acted on nowhere here.
        AgentEvent::UserMessage { .. } | AgentEvent::Other => {}
    }
}

/// A colony proposed a shared-memory note: queue it for review (or store it when review is off).
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
    let title = note.title.clone();
    let stored = if orgs::memory_requires_review(&modules) {
        app.memory.add_proposal(note).await.map(|proposal| json!(proposal))
    } else {
        match memory::store_note(app, note.clone()).await {
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
                if waiting {
                    ", waiting for your review"
                } else {
                    " (review is off, so it is live)"
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

/// A colony's orchestrator confirmed something outside its task: file it as an issue on the
/// colony's repository, unless findings are off, the colony has hit its cap, or an open issue
/// already has the same title. Every outcome is recorded and logged; none reaches the agent.
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
            json!({"title": finding.title, "issue": url}),
        ),
        Ok(findings::Filed::Duplicate(url)) => (
            "info",
            format!("did not file \"{}\": {url} is already open with that title", finding.title),
            json!({"title": finding.title, "duplicate_of": url}),
        ),
        Err(e) => (
            "error",
            format!("could not file finding \"{}\": {e:#}", finding.title),
            Value::Null,
        ),
    };
    // Only a filed or matched finding counts toward the cap; a GitHub error should not use one up.
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
}
