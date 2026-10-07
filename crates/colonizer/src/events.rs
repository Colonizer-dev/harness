//! The live link to a colony's agent: one websocket to agentd per session, reconnecting with the
//! last seen `seq`, turning agent events into session state, autopilot decisions, memory proposals
//! and findings.
//!
//! The autopilot decision itself is a pure function (`autopilot_step`) so the policy can be tested
//! apart from the stream it acts on.

use crate::{Shared, findings, github, memory, orgs, provider_quota, spend};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self};

use crate::protocol::{AgentEvent, AgentState, Origin, QuestionRisk};
#[allow(unused_imports)]
use crate::{lifecycle::*, publish::*, queue::*, sessions::*};

#[derive(Debug, PartialEq)]
pub(crate) enum Autopilot {
    Publish,
    Wait(&'static str),
    /// Flags the colony for the maintainer.
    Hold(&'static str),
    /// Schedule an automatic continue after a transient provider error (issue #980), releasing the
    /// slot while it backs off; only when the retries run out does it become a [`Autopilot::Hold`].
    Retry(&'static str),
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

/// How long after a mothership restart reconnects to a running colony a turn that ends on a model
/// gateway error is read as the restart's doing and continued once, straight away (issue #1093).
pub(crate) const RESTART_RESUME_WINDOW: chrono::Duration = chrono::Duration::minutes(15);

/// The message that continues a colony whose turn the restart cut off (issue #1093).
const RESTART_RESUME_MESSAGE: &str = "Your last turn stopped on a model gateway error while the mothership restarted. \
     Nothing was wrong with your work: continue where you left off.";

/// The `attention.cause` of a colony stopped on a transient model gateway error (issue #1093): the
/// cockpit names the error and offers Retry instead of asking for an answer nobody is waiting on.
pub(crate) const GATEWAY_ERROR_CAUSE: &str = "gateway_error";

/// The `attention.cause` of a colony held because its turn ended on an error that is not transient.
pub(crate) const TURN_ERROR_CAUSE: &str = "turn_error";

/// The `attention.cause` of a colony held because its fresh-checkout verification never got to the
/// tests — a toolchain or registry download, a DNS lookup failed — and the retries ran out (issue
/// #1117). Not "the completion claim was contradicted": no test ran, so nothing contradicts it.
pub(crate) const VERIFY_NETWORK_CAUSE: &str = "verify_network";

/// "a model gateway error (502, connection to Anthropic)", or without the parentheses when the
/// error named neither a status nor a failure.
fn gateway_error_phrase(cause: &str) -> String {
    if cause.is_empty() {
        "a model gateway error".into()
    } else {
        format!("a model gateway error ({cause})")
    }
}

/// The attention a colony parks with while it waits out an automatic retry (issue #1093): the
/// reason stays `provider_retry` — nobody has to act — and `cause`, `summary` (what stopped it),
/// `detail` (that, with the attempt) and `retry_at` let the card say what stopped it and when it
/// goes again.
pub(crate) fn gateway_retry_attention(cause: &str, attempt: u32, max_attempts: u64, retry_at: DateTime<Utc>) -> Value {
    json!({
        "reason": PROVIDER_RETRY_REASON,
        "since": Utc::now(),
        "nudges": 0,
        "cause": GATEWAY_ERROR_CAUSE,
        "summary": format!("Stopped on {}", gateway_error_phrase(cause)),
        "detail": format!(
            "Stopped on {}: retrying automatically (attempt {attempt} of {max_attempts})",
            gateway_error_phrase(cause)
        ),
        "retry_at": retry_at,
        "attempt": attempt,
        "max_attempts": max_attempts,
    })
}

/// The hold once the automatic retries are spent, or when they are off (issue #1093).
pub(crate) fn gateway_held_attention(cause: &str, max_attempts: u64) -> Value {
    let detail = match max_attempts {
        0 => format!("Stopped on {}; automatic retry is off", gateway_error_phrase(cause)),
        1 => format!(
            "Stopped on repeated gateway errors{}; the automatic retry did not get through",
            if cause.is_empty() {
                String::new()
            } else {
                format!(" ({cause})")
            }
        ),
        n => format!(
            "Stopped on repeated gateway errors{}; {n} automatic retries did not get through",
            if cause.is_empty() {
                String::new()
            } else {
                format!(" ({cause})")
            }
        ),
    };
    json!({"reason": AUTOPILOT_HELD_REASON, "since": Utc::now(), "nudges": 0, "cause": GATEWAY_ERROR_CAUSE, "detail": detail})
}

/// The hold for a turn that ended on an error a retry cannot fix (issue #1093): the detail is the
/// error's first line, so the card says what happened instead of that the watchdog flagged it.
pub(crate) fn turn_error_held_attention(error: Option<&str>) -> Value {
    let first = error.and_then(|e| e.lines().map(str::trim).find(|l| !l.is_empty()));
    let detail = match first {
        Some(line) if line.chars().count() > 200 => {
            format!("Stopped on an error: {}…", line.chars().take(200).collect::<String>())
        }
        Some(line) => format!("Stopped on an error: {line}"),
        None => "The agent's turn ended with an error".into(),
    };
    json!({"reason": AUTOPILOT_HELD_REASON, "since": Utc::now(), "nudges": 0, "cause": TURN_ERROR_CAUSE, "detail": detail})
}

/// The hold for a claim whose verification could not run for the network (issue #1117): the card says
/// "verification could not run (network: rustup toolchain download, connection reset)".
pub(crate) fn verify_network_held_attention(cause: &str) -> Value {
    let detail = format!("verification could not run (network: {cause})");
    json!({"reason": AUTOPILOT_HELD_REASON, "since": Utc::now(), "nudges": 0, "cause": VERIFY_NETWORK_CAUSE, "detail": detail})
}

/// What autopilot does when a turn ends; writing `pr.md` during the turn is the agent's signal that it's done.
fn autopilot_step(errored: bool, transient: bool, interrupted: bool, open_question: bool, pr_written: bool) -> Autopilot {
    if open_question {
        Autopilot::Wait("a question is open")
    } else if interrupted {
        Autopilot::Wait("the turn was interrupted")
    } else if errored && transient {
        Autopilot::Retry("the agent's turn ended with a transient provider error")
    } else if errored {
        Autopilot::Hold("the agent's turn ended with an error")
    } else if !pr_written {
        Autopilot::Wait("the agent didn't write or update its PR description this turn")
    } else {
        Autopilot::Publish
    }
}

/// Issue #328: what autopilot does once a completion claim's verification verdict is in. A
/// contradicted colony is held for the maintainer exactly as a failed turn is; anything else
/// publishes as before — an unverifiable claim is not the colony's fault, and holding it would
/// strand finished work on infra noise, and an inconclusive one failed on the base commit too, so
/// the failure is not this change's.
pub(crate) fn verdict_step(verdict: &crate::verify::Verdict) -> Autopilot {
    match verdict {
        crate::verify::Verdict::Contradicted => Autopilot::Hold("the completion claim was contradicted"),
        crate::verify::Verdict::Confirmed | crate::verify::Verdict::Inconclusive | crate::verify::Verdict::Unverifiable => {
            Autopilot::Publish
        }
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

/// Which subsystem a runner line came from: the closed `origin` the host stamps onto it before
/// persisting (docs/protocol.md §3). Pure, so every branch is pinned by a test; what cannot be read
/// off the line is handed in — the session's launch tag as `launch`, and, for a `question_answered`,
/// whether the judge is the one who answered as `judged`. Anything unrecognisable stays a runner
/// line (`agent`): an origin is provenance, never a contract a line can fail.
pub fn resolve_origin(event: &Value, launch: Option<&str>, judged: bool) -> Origin {
    // A subagent's events carry the `agent` ref (§2 rules); the ref is the tell, whatever the type.
    if event.get("agent").is_some() {
        return Origin::Subagent;
    }
    match event["type"].as_str() {
        // The echo of an accepted message tells its senders apart by id: the watchdog's own nudges
        // (`watchdog-`, §6.3), the brief the session launched with (`initial`), anyone else a person.
        Some("user_message") => match event["id"].as_str() {
            Some(echoed) if echoed.starts_with("watchdog-") => Origin::Watchdog,
            Some("initial") => launch_origin(launch),
            _ => Origin::User,
        },
        // The judge's answer and a person's arrive as the same echo, so the judge records the ids it
        // answered (autonomy.rs) and the handler spends that record here. Spent: a replay of the
        // same echo — or one still in flight across a mothership restart — reads as the person's.
        Some("question_answered") if judged => Origin::Autonomy,
        Some("question_answered") => Origin::User,
        _ => Origin::Agent,
    }
}

/// The subsystem a session was launched by, read off its `Session.origin` tag. The event vocabulary
/// distinguishes only the machine launchers it has; a person's colony — and a `map` colony, whose
/// brief the orchestrator wrote — reads as a plain user's.
fn launch_origin(launch: Option<&str>) -> Origin {
    match launch {
        Some("burn_down") => Origin::BurnDown,
        Some(crate::redteam::REDTEAM_ORIGIN) => Origin::Redteam,
        _ => Origin::User,
    }
}

/// Whether a runner line counts as progress for the watchdog (§6.3). Status changes never are, a
/// `model_changed` is a switch rather than work, and lines the host's own helpers caused — a
/// watchdog nudge, a judge answer — would let a colony stall-proof itself by talking to itself.
/// Everything else — the agent working, its person stepping in — is.
fn is_watchdog_progress(origin: Origin, kind: &str) -> bool {
    // A `boundary` event is a control refusing something (issue #609), never work.
    !matches!(kind, "status" | "model_changed" | "boundary") && !matches!(origin, Origin::Watchdog | Origin::Autonomy)
}

/// Tracks the shape of the turn the runner is in, for the watchdog's turn-end recovery (issue #878):
/// when it last spoke its final answer, and which tool calls it has opened and not answered. Read off
/// the raw line, like [`resolve_origin`], because most of these types never reach the dispatch below.
/// A final `assistant_text` is the colony's "it's done"; every later sign of work clears it, so a
/// stale one means the runner said it was finished and then went quiet. Only the lead agent's own
/// text counts — a subagent's final block is its own turn's — and only pure telemetry is left
/// standing, so the lines the runner emits around a withheld `turn_end` cannot fake an end.
async fn note_turn_shape(rt: &Runtime, event: &Value) {
    let kind = event["type"].as_str().unwrap_or_default();
    match kind {
        "tool_call" => {
            if let Some(id) = event["tool_call_id"].as_str() {
                rt.open_tool_calls.lock().await.insert(id.to_string());
            }
        }
        "tool_result" => {
            if let Some(id) = event["tool_call_id"].as_str() {
                rt.open_tool_calls.lock().await.remove(id);
            }
        }
        "turn_end" => rt.open_tool_calls.lock().await.clear(),
        _ => {}
    }
    let mut final_text = rt.final_text_at.lock().await;
    match kind {
        // The lead agent's final, non-delta answer is the colony's "it's done". A delta is streaming,
        // not final (§2), and a subagent's text is its own turn's, not the colony's answer — both
        // fall through to clear below, which is right: the lead's turn is still moving.
        "assistant_text" if event.get("agent").is_none() => *final_text = Some(Utc::now()),
        // Pure telemetry that cannot mean new work, left standing so it cannot erase a "done" the
        // wedge came after. The choice-card re-ask is the case that matters: the runner withholds
        // `turn_end` on purpose and writes only a `log` line and a status, so a `log` must not clear
        // it while a `status: working` must (below). The rest are the runner's forwarding-only
        // telemetry: the session id, the model change, the path-policy report, the Jev ladder.
        "log" | "model_changed" | "path_policy" | "boundary" | "agent_session" | "jev_ladder" => {}
        // A status other than `working` is lifecycle, not work: idle before the next message, an
        // exit the status path already handles.
        "status" if event["state"].as_str() != Some("working") => {}
        // Everything else — a delta, a thought, a tool call or result, a question, a `status:
        // working`, a fresh `user_message` opening a new turn, or any type this build does not know —
        // is the turn still moving. A wedged runner emits nothing at all, so clearing on anything
        // ambiguous costs no recovery: the last event before the silence is what we read.
        _ => *final_text = None,
    }
}

/// The mothership-log line for a colony's model-router `log` event (issue #983), `None` for any other
/// event. The router names the provider, the failure class, the status and the elapsed time; this adds
/// the colony and the Claude account it was launched with — the account's id, never its credential. The
/// event arrives already redacted; the line is kept to one line and a bounded length, since a guest
/// writes it.
pub(crate) fn model_router_line(id: &str, account: Option<&str>, event: &Value) -> Option<String> {
    if event["type"] != "log" || event["source"] != "model_router" {
        return None;
    }
    let message: String = event["message"]
        .as_str()?
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(500)
        .collect();
    let level = event["level"]
        .as_str()
        .filter(|level| matches!(*level, "info" | "warn" | "error"))
        .unwrap_or("info");
    Some(format!(
        "model router: colony {id} account={} [{level}] {message}",
        account.unwrap_or("default")
    ))
}

pub(crate) async fn handle_agent_event(app: &Shared, id: &str, rt: &Arc<Runtime>, line: &str) {
    let Ok(mut event) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let Some(seq) = event["seq"].as_u64() else { return };
    // The origin is resolved before the append, so the persisted line and the broadcast every open
    // browser sees both carry it (docs/protocol.md §3). One event keeps its body field: a
    // memory_proposal's `origin` names the proposer (§6.2) and predates the envelope, so its lines
    // are never stamped — an absent body origin must stay absent (it reads as the orchestrator's),
    // and the dispatch below reads the proposer, not a stamp.
    let launch = app.session(id).await.and_then(|s| s.origin.clone());
    let judged = match event["type"].as_str() {
        Some("question_answered") => match event["question_id"].as_str() {
            Some(question_id) => rt.judged_questions.lock().await.remove(question_id),
            None => false,
        },
        _ => false,
    };
    let origin = resolve_origin(&event, launch.as_deref(), judged);
    if event["type"] != "memory_proposal" {
        // A line arriving with an `origin` of its own is speaking outside its contract: the envelope
        // is the host's, and the resolved stamp below overwrites whatever it carried. The carried
        // value still goes through [`Origin::parse_logged`] first, so a value outside the closed
        // vocabulary — a writer's bug, §3 — is named out loud before it is replaced.
        if let Some(carried) = event.get("origin").and_then(Value::as_str) {
            Origin::parse_logged(Some(carried));
        }
        event["origin"] = json!(origin.as_str());
    }
    // A credential the agent echoed or a tool printed is redacted field by field before the line is
    // persisted or broadcast (#761): the disk, an archive and a fleet export only ever see the mark.
    crate::redact::redact_value(&mut event);
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
        // does not collide is appended as the runner wrote it plus the host's `origin` stamp — the
        // one re-serialisation the host makes on every line (agentd's own output is sorted-key JSON
        // too, so nothing else moves).
        let (file_seq, file_line);
        // Through the store, so the line lands in `sessions/<id>/events.jsonl` by the same name a
        // remote backend would answer by; the store adds the newline, as `append_line` did.
        let err = if seq <= rt.last_seq.load(Ordering::SeqCst) {
            event["seq"] = json!(rt.last_seq.load(Ordering::SeqCst) + 1);
            event["a_seq"] = json!(seq);
            file_seq = event["seq"].as_u64().unwrap_or(seq);
            file_line = event.to_string();
            app.store()
                .append(id, "events.jsonl", file_line.as_bytes())
                .await
                .map_err(anyhow::Error::from)
                .err()
        } else {
            file_seq = seq;
            file_line = event.to_string();
            app.store()
                .append(id, "events.jsonl", file_line.as_bytes())
                .await
                .map_err(anyhow::Error::from)
                .err()
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
        app.session_log(id, "error", format!("could not append to events.jsonl: {e:#}"))
            .await;
    }
    rt.broadcast(Some(file_seq), file_line.to_string());

    // Router upstream failures and fallbacks reach the mothership's own output too (#983): they were
    // only in the colony's event log, so a run of "Anthropic is unreachable" left nothing to read there.
    if event["type"] == "log" && event["source"] == "model_router" {
        let account = app
            .session(id)
            .await
            .map(|s| crate::account_health::account_of(&s))
            .unwrap_or_else(|| "default".into());
        if let Some(line) = model_router_line(id, Some(&account), &event) {
            eprintln!("{line}");
        }
        // Issue #984: an upstream failure on the account's own Anthropic path marks the account, so
        // its colonies wait on it (finish_turn below) and the owner is told once. Gateway-routed
        // requests name their provider and are not this account's traffic.
        if let Some((status, _)) = crate::account_health::router_failure(&event) {
            crate::account_health::record_failure(app, &account, status).await;
        }
    }

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

    // Progress for the watchdog, by origin (§6.3): status changes and a `model_changed` are never
    // progress, and neither is anything the host's own helpers said — a watchdog nudge or a judge
    // answer would otherwise stall-proof a colony that is only talking to itself. A person's
    // message and the agent working count, as before.
    if is_watchdog_progress(origin, event["type"].as_str().unwrap_or_default()) {
        // A denied tool result is not progress (issue #609): the colony is circling a boundary it
        // cannot cross, so the result neither restarts the stall clock nor spends a nudge, and its
        // streak is on record for `decide` and the nudge. A successful result ends the streak, and
        // so does anything that is a real break in the loop — a person's message, a question, a
        // turn end. While the streak stands even real work — a retried call, a line of text — must
        // not spend the nudges, or the loop would stall-proof itself: `last` still moves, so the
        // cockpit's activity stamp stays truthful, and `decide` reads `denied_since` instead.
        let progress = {
            let mut activity = rt.activity.lock().await;
            let kind = event["type"].as_str().unwrap_or_default();
            let denied = crate::watchdog::note_denials(&mut activity, kind, &event);
            if !denied {
                activity.last = Utc::now();
            }
            let looping = activity.denials >= crate::watchdog::HINT_LOOP_DENIALS;
            if !denied && !looping {
                activity.nudges = 0;
                activity.last_nudge = None;
            }
            !denied && !looping
        };
        // A control-defeat flag (issue #609) is not lifted by the colony carrying on: only a
        // person's own message, which says someone has looked, clears it.
        let person_spoke = origin == Origin::User && event["type"] == "user_message";
        let clears = |attention: &Value| attention["reason"] != crate::watchdog::CONTROL_DEFEAT_REASON || person_spoke;
        if progress
            && app
                .session(id)
                .await
                .is_some_and(|s| s.attention.as_ref().is_some_and(clears))
        {
            app.update_session(id, |x| x.attention = None).await;
        }
    }

    // Jev visibility ladder, stage 1 (#475): every tool call is both the evidence a pending decision
    // waits for (a re-issue of an earlier call) and the next original a later decision may name.
    // tool_call is a forwarded-only type — it never reaches the match below — so this reads it off
    // the raw line, the way `resolve_origin` does.
    if event["type"] == "tool_call" {
        crate::jev_ladder::note_tool_call(app, id, rt, &event).await;
    }
    // Jev brief picks (#585): a watched note's guest path in a call's input or a result's output, or
    // a `Skill` call naming a watched pack, marks the item used. Shadow measurement only, and a
    // no-op unless the boot picker armed a watch.
    crate::brief_pick::note_event(app, id, rt, &event).await;
    // The control-defeat watch (issue #609): a tool call naming a target a control refused is
    // held, and a successful result for it is the deny-then-reach signature.
    if matches!(event["type"].as_str(), Some("tool_call" | "tool_result")) {
        crate::boundary::observe_tool(app, id, rt, &event).await;
    }
    // The shape of the turn, read off the raw line for the watchdog's turn-end recovery (issue
    // #878): a tool call in flight, or a final answer whose turn_end never came.
    note_turn_shape(rt, &event).await;

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
            // A suspended colony (issue #562) is not taking its runner's word any more: its link is
            // draining on the way down, and a straggler status — Working, Idle, Exited — must not
            // flip the record out of `waiting_for_answer`, or the restore pass would never pick the
            // held answer up. The teardown was planned, so an `exited` here is no failure either.
            if let Some(current) = app.session(id).await
                && current.status.is_live()
                && current.suspended.is_none()
                && (current.status != next || error.is_some())
            {
                let became_idle = next == SessionStatus::Idle && current.status != SessionStatus::Idle;
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
                // A mapping colony that just went idle may be done: its map is picked up and it
                // stops itself (maps.rs), so it does not hold a parallel slot. Spawned: the stop takes
                // the colony's lifecycle lock, which this event loop must not wait on.
                if became_idle && current.origin.as_deref().is_some_and(crate::maps::is_map_origin) {
                    tokio::spawn(crate::maps::on_idle(app.clone(), id.to_string()));
                }
            }
        }
        AgentEvent::Question {
            question_id,
            questions,
            risk,
            kind,
            blocking,
            ..
        } => {
            // The questions travel with the id: autonomous mode answers among the options the
            // agent offered, and nothing else (docs/protocol.md §6.2b). The risk class travels
            // too — the judge answers only at or below its ceiling — and a question without one,
            // from an older runner, counts as a workspace write.
            let risk = risk.unwrap_or(QuestionRisk::WorkspaceWrite);
            // Before the question opens, so a suspension tick that sees it open reads its kind too.
            rt.question_holds_tool_call.store(
                crate::protocol::question_holds_tool_call(kind.as_deref(), blocking),
                std::sync::atomic::Ordering::SeqCst,
            );
            // A new question retires the old one's notification answer tokens (issue #742).
            app.answer_tokens.revoke(id).await;
            *rt.open_question.lock().await = Some((question_id, questions, risk));
            rt.activity.lock().await.question_since = Some(Utc::now());
        }
        AgentEvent::QuestionAnswered { question_id, .. } => {
            // Only the question we are actually tracking closes here (issue #981). The slot holds one
            // question, and a subagent's ask can arrive after the lead's and replace it; an answer to
            // that earlier question — echoed back now — must not take the tracked, still-open one down
            // with it. Clearing it left the colony's live question untracked, so `ask`/`answer` saw
            // nothing pending while the status still read `waiting_for_answer`. The replay in
            // `Runtime::load` already pairs by id; this mirrors it.
            let matched = {
                let mut open = rt.open_question.lock().await;
                if open.as_ref().is_some_and(|(open_id, ..)| open_id == &question_id) {
                    *open = None;
                    true
                } else {
                    false
                }
            };
            if matched {
                rt.question_holds_tool_call.store(false, std::sync::atomic::Ordering::SeqCst);
                let mut activity = rt.activity.lock().await;
                activity.question_since = None;
                // The question is resolved either way, so an unanswered-provider streak behind it is over.
                activity.judge_failures = 0;
            }
            // The notification answer tokens are keyed by the session, not the question: any of its
            // questions being answered retires them, whether or not it is the one we are tracking.
            app.answer_tokens.revoke(id).await;
        }
        AgentEvent::AgentSession { session_id } => {
            // The runner's own session id (issue #562), kept so a colony suspended while it waits
            // on its user can come back and resume this same conversation. A write that changes
            // nothing — an init re-reporting the id it already gave — persists and broadcasts
            // nothing.
            app.update_session(id, |x| x.agent_session = Some(session_id)).await;
        }
        AgentEvent::MemoryProposal {
            scope,
            title,
            content,
            tags,
            origin,
            kind,
            confidence,
        } => {
            let proposal = ProposalBody {
                scope: scope.as_deref(),
                title: &title,
                content: &content,
                tags: &tags,
                kind: kind.as_deref(),
                confidence,
            };
            memory_proposal_full(app, id, origin.as_deref(), proposal).await;
        }
        AgentEvent::VaultProposal {
            path,
            title,
            body,
            reason,
        } => {
            let proposal = crate::vault::Draft {
                path: &path,
                title: &title,
                body: &body,
                reason: &reason,
            };
            crate::vault::propose(app, id, origin, proposal).await;
        }
        // Spawned: filing talks to GitHub, and the colony's event stream should not wait on it.
        AgentEvent::Finding { .. } => {
            tokio::spawn(file_finding(app.clone(), id.to_string(), rt.clone(), event.clone()));
        }
        // Spawned for the same reason: a GitHub-needing loop's write is a host-side `gh` call
        // (loop_github.rs), validated, capped per colony and held back by the write kill-switch.
        AgentEvent::GithubAction { .. } => {
            tokio::spawn(crate::loop_github::perform(
                app.clone(),
                id.to_string(),
                rt.clone(),
                event.clone(),
            ));
        }
        AgentEvent::LoopNext { delay_minutes, reason } => {
            crate::loops::on_next(app, id, delay_minutes, &reason).await;
        }
        AgentEvent::LoopStop { reason } => {
            crate::loops::on_stop(app, id, &reason).await;
        }
        // The path policy's runtime report (issue #647): a log line and an activity entry per
        // distinct (access, path), with the untrusted fields sanitised on this side (path_policy.rs).
        AgentEvent::PathPolicy {
            access,
            policy,
            path,
            tool,
        } => {
            crate::path_policy::on_attempt(app, id, rt, &access, &policy, &path, &tool).await;
        }
        // A control refused something (issue #609): the runner's or agentd's boundary record, cleaned
        // on this side and folded into the watchdog's control-defeat signature.
        AgentEvent::Boundary { .. } => {
            if let Some(boundary) = crate::boundary::Boundary::from_event(&event) {
                crate::boundary::observe(app, id, boundary).await;
            }
        }
        // Shadow measurement (#475): the pass's decisions are logged to the jev_ladder ledger and
        // arm the reread watch, and nothing else changes. Pure telemetry — never read as acted on.
        AgentEvent::JevLadder { applied, decisions, .. } => {
            crate::jev_ladder::on_ladder(app, id, rt, applied, &decisions).await;
        }
        AgentEvent::TurnEnd {
            is_error,
            result,
            cost_usd,
            model_usage,
            ..
        } => finish_turn(app, id, rt, is_error, result, cost_usd, model_usage).await,
        // Forwarded to the browser above and acted on nowhere here.
        AgentEvent::UserMessage { .. } | AgentEvent::Other => {}
    }
}

/// The effects of a turn ending — spend accounting, the budget check and the autopilot publish
/// decision — split out of the dispatch so the watchdog can run them for a turn the runner never
/// ended (issue #878, `watchdog::maybe_finish_turn`). `cost_usd`/`model_usage` are the real turn's
/// cumulative figures, or `None` for a synthetic end, whose measured difference is zero and so
/// moves no spend and re-adds nothing.
pub(crate) async fn finish_turn(
    app: &Shared,
    id: &str,
    rt: &Arc<Runtime>,
    is_error: bool,
    result: Option<String>,
    cost_usd: Option<f64>,
    model_usage: Option<Value>,
) {
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
        spend::record_turn_usage(app, &s, old_cost, old_usage.as_ref(), cost, s.model_usage.as_ref()).await;
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
        let errored = is_error;
        // Issue #1093: every turn end spends the restart resume, so it only ever covers the first
        // turn to end after the restart reconnected to this colony.
        let restart_resume = rt.take_restart_resume(Utc::now());
        // Issue #984: a turn that died because the colony's Claude account is unusable — the account
        // is already marked, or the result names a 401/403 sign-in failure — waits on the account
        // instead of being held or retried. The slot is released and no `provider_retries` are spent;
        // the queue resumes the colony once the account works again.
        if errored
            && s.status.is_live()
            && let Some(account) = crate::account_health::unusable_account(app, &s, result.as_deref()).await
        {
            crate::account_health::park_waiting(app, &s, &account).await;
            return;
        }
        // Issue #980: a turn that died on a blip the retry classifier calls transient is retried
        // automatically rather than held. Anything else keeps the old hold. The turn's own free-text
        // result carries the provider's message ("API Error: 502 model router: ...").
        let transient = errored
            && result.as_deref().is_some_and(|r| {
                matches!(
                    crate::retry::classify_turn_error(r),
                    crate::retry::FailureClass::TransientInfra
                )
            });
        // Issue #1093: a turn the mothership's own restart cut off — its gateway socket dropped —
        // is continued once, straight away, without parking the colony or spending a retry.
        if transient && restart_resume && s.status.is_live() && !interrupted {
            let error_text = result.clone().unwrap_or_default();
            app.session_log(
                id,
                "warn",
                format!(
                    "the turn stopped on {} while the mothership restarted; continuing once automatically",
                    gateway_error_phrase(&crate::retry::turn_error_cause(&error_text))
                ),
            )
            .await;
            crate::recovery::send_user_message(rt, "recovery", RESTART_RESUME_MESSAGE);
            return;
        }
        // A turn that ends cleanly clears any backoff from an earlier error, so a later unrelated
        // one starts its own sequence instead of inheriting spent attempts.
        if !errored && s.provider_retries != 0 {
            app.update_session(id, |x| x.provider_retries = 0).await;
        }
        let open_question = rt.open_question.lock().await.is_some();
        let step = autopilot_step(errored, transient, interrupted, open_question, pr_written);
        // #761: a description that redaction changed is published only after a person has
        // looked — the colony had a secret in hand, and the diff is not redacted.
        let secret_note = (step == Autopilot::Publish)
            .then(|| github::pr_description_secret_note(&app.session_dir(id).join("out"), &s))
            .flatten();
        if s.autopilot && s.status.is_live() {
            match step {
                Autopilot::Publish if secret_note.is_some() => {
                    let note = secret_note.as_deref().unwrap_or_default();
                    app.session_log(
                        id,
                        "warn",
                        format!("autopilot: not publishing, {note}; press Create PR when the work is ready"),
                    )
                    .await;
                    app.update_session(id, |x| {
                        x.attention = Some(json!({"reason": "autopilot_held", "since": Utc::now(), "nudges": 0}));
                    })
                    .await;
                    tokio::spawn(crate::verify::after_turn(app.clone(), id.to_string(), false));
                }
                // Issue #84: the kill-switch holds the publish without flagging the colony.
                Autopilot::Publish if crate::authority::external_writes_blocked() => {
                    app.session_log(id, "warn", AUTOPILOT_BLOCKED.into()).await;
                    tokio::spawn(crate::verify::after_turn(app.clone(), id.to_string(), false));
                }
                Autopilot::Publish => {
                    app.session_log(
                        id,
                        "info",
                        "autopilot: the agent finished and wrote its PR description; verifying the claim \
                                 before publishing"
                            .into(),
                    )
                    .await;
                    tokio::spawn(crate::verify::after_turn(app.clone(), id.to_string(), true));
                }
                Autopilot::Wait(reason) => {
                    app.session_log(id, "info", format!("autopilot: not publishing yet, {reason}"))
                        .await
                }
                // Issue #980: a transient provider error is retried automatically instead of held.
                // Each attempt parks the colony — releasing its parallel slot — until the backoff
                // step passes (queue.rs). Once the attempts run out it is held like any other error,
                // naming the provider's own message rather than telling the operator to press
                // Create PR when there is no work behind the failure.
                Autopilot::Retry(reason) => {
                    let modules = app.modules.read().await.clone();
                    let max_attempts = crate::orgs::provider_retry_max_attempts(&modules);
                    let schedule = crate::orgs::provider_retry_schedule(&modules);
                    let error_text = result.clone().unwrap_or_default();
                    let cause = crate::retry::turn_error_cause(&error_text);
                    if (s.provider_retries as u64) < max_attempts {
                        let attempt = s.provider_retries + 1;
                        let delay = crate::orgs::provider_retry_delay(&schedule, attempt);
                        let delay_minutes = delay.num_minutes();
                        app.session_log(
                            id,
                            "warn",
                            format!(
                                "autopilot: {reason} ({error_text}); retrying automatically — attempt {attempt}/{max_attempts}, next in {delay_minutes} min"
                            ),
                        )
                        .await;
                        app.update_session(id, |x| x.provider_retries = attempt).await;
                        let parked = crate::lifecycle::park_colony(
                            app,
                            &s,
                            PROVIDER_RETRY_REASON,
                            None,
                            format!("provider error, retrying automatically (attempt {attempt}/{max_attempts})"),
                            format!("provider error, retrying in {delay_minutes} min (attempt {attempt}/{max_attempts})"),
                        )
                        .await;
                        // Issue #1093: the park's bare attention gains what the card needs to name the
                        // error and the next attempt — only while the park is still the retry's.
                        if parked {
                            let retry_at = Utc::now() + delay;
                            app.update_session(id, |x| {
                                if x.parked.as_ref().is_some_and(|p| p.reason == PROVIDER_RETRY_REASON) {
                                    x.attention = Some(gateway_retry_attention(&cause, attempt, max_attempts, retry_at));
                                }
                            })
                            .await;
                        }
                    } else {
                        app.session_log(
                            id,
                            "warn",
                            format!(
                                "autopilot: not publishing, {reason} after {max_attempts} automatic retries; last error: {error_text}"
                            ),
                        )
                        .await;
                        app.update_session(id, |x| {
                            x.attention = Some(gateway_held_attention(&cause, max_attempts));
                            x.provider_retries = 0;
                        })
                        .await;
                    }
                }
                Autopilot::Hold(reason) => {
                    // The recovery point (issue #586): holding for a person is the rule, and
                    // when the point is on Jev may answer with a retry or a narrower task
                    // instead. Off leaves this arm exactly as it was. A colony the gateway
                    // already flagged as a provider error is that class, not a plain hold.
                    let failure = if s.attention.as_ref().and_then(|a| a["reason"].as_str()) == Some("model_error") {
                        crate::recovery::Failure::ProviderError
                    } else {
                        crate::recovery::Failure::AutopilotHeld
                    };
                    let (action, _) = crate::recovery::handle(app, &s, failure, "ask_human", false).await;
                    // An option with a message talks to the agent instead of holding — the
                    // message sets the next turn's work, and no attention flag goes up. An
                    // option without one (`ask_human`, `stop`) holds as it always did.
                    if let Some(text) = action.agent_message() {
                        crate::recovery::send_user_message(rt, "recovery", text);
                        app.session_log(
                            id,
                            "info",
                            format!(
                                "autopilot: Jev chose {} instead of holding; sent the agent a message",
                                action.as_str()
                            ),
                        )
                        .await;
                    } else {
                        app.session_log(
                            id,
                            "warn",
                            format!("autopilot: not publishing, {reason}; press Create PR when the work is ready"),
                        )
                        .await;
                        app.update_session(id, |x| {
                            x.attention = Some(turn_error_held_attention(result.as_deref()));
                        })
                        .await;
                    }
                }
            }
        } else if step == Autopilot::Publish && s.status.is_live() {
            // Issue #328: autopilot off still verifies and records the claim; publishing stays manual.
            tokio::spawn(crate::verify::after_turn(app.clone(), id.to_string(), false));
        }
    }
    // A turn that died on an empty plan parks the colony instead of holding it: the error text
    // is the only copy of the provider's answer the colony side ever sees.
    if is_error
        && let Some(text) = result.as_deref()
        && let Some(hit) = provider_quota::classify_quota_exhaustion(0, "", text)
    {
        park_quota_colony(app, id, text, &hit).await;
    }
}

/// Parks a colony whose turn died on an exhausted provider (issue #213): the slot is released, the
/// worktree and branch kept, the park record carries the reason and the upstream reset time, and
/// the attention flag the cockpit's banner reads is the #230 contract. What happens to the
/// microVM — discarded, or kept idle when `resume.discard_vm` is off or the worktree could not be
/// verified — is `park_colony`'s decision.
async fn park_quota_colony(app: &Shared, id: &str, text: &str, hit: &provider_quota::QuotaExhaustion) {
    let providers = app.providers();
    let ids: Vec<String> = providers.iter().map(|p| p.id.clone()).collect();
    let names: Vec<String> = providers.iter().map(|p| p.name.clone()).collect();
    let provider = provider_quota::mentioned_provider(text, &ids, &names);
    if let Some(pid) = &provider {
        app.gateway.mark_quota_exhausted(pid, hit.reset_at.clone(), hit.reset_unix);
    } else if hit.account_wide {
        // The Claude account's own cap names no provider — the colony side never learns one — so
        // the park records it on the dedicated account record instead of any real provider: healthy
        // providers stay healthy, and the queue pauses on the account record alone.
        app.gateway.mark_account_quota_exhausted(hit.reset_at.clone(), hit.reset_unix);
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
    // The account fallback could not carry this colony's task (#1130): say why, so the card reads
    // "needs a trusted provider: Claude is out until 19:51; MiniMax is not marked trusted".
    let error = match app
        .gateway
        .account_park_reason(id)
        .filter(|_| provider.is_none() && hit.account_wide)
    {
        Some(reason) => format!("{error}; {reason}"),
        None => error,
    };
    park_colony(
        app,
        &s,
        provider_quota::QUOTA_EXHAUSTED_REASON,
        hit.reset_at.clone(),
        error,
        "provider quota exhausted; parked, the worktree kept — the colony resumes when the plan refills".into(),
    )
    .await;
}

/// A proposal's body, as the runner sent it.
pub(crate) struct ProposalBody<'a> {
    pub scope: Option<&'a str>,
    pub title: &'a str,
    pub content: &'a str,
    pub tags: &'a [String],
    pub kind: Option<&'a str>,
    pub confidence: Option<f64>,
}

/// A proposal without a kind or confidence, as runners from before issue #766 send it.
#[cfg(test)]
pub(crate) async fn memory_proposal(
    app: &Shared,
    id: &str,
    origin: Option<&str>,
    scope: Option<&str>,
    title: &str,
    content: &str,
    tags: &[String],
) {
    let body = ProposalBody {
        scope,
        title,
        content,
        tags,
        kind: None,
        confidence: None,
    };
    memory_proposal_full(app, id, origin, body).await
}

/// A colony proposed a shared-memory note: queue it for review (or, for a repo note with review off,
/// store it marked unreviewed). A proposal from anyone but the orchestrator is refused before any
/// store is touched, so with the `mem0` provider nothing reaches mem0 either (§6.2). A global
/// proposal is only a sighting of a fleet-wide candidate (issue #766): it is queued for review as a
/// global note once candidates from enough distinct repositories agree with enough confidence.
pub(crate) async fn memory_proposal_full(app: &Shared, id: &str, origin: Option<&str>, body: ProposalBody<'_>) {
    let ProposalBody {
        scope,
        title,
        content,
        tags,
        kind,
        confidence,
    } = body;
    let Some(s) = app.session(id).await else { return };
    let scope = scope.unwrap_or("repo");
    // Shared memory is read-only from inside a colony (docs/architecture.md, "Shared memory
    // access"): only the orchestrator proposes, checked here in front of the org's memory switch,
    // which is about what is kept, not who may ask. An absent `origin` is a runner from before the
    // field existed, and only the orchestrator could reach the tool then.
    let role = match origin {
        None => Some(memory::Role::Orchestrator),
        Some(origin) => memory::role_of(origin),
    };
    if !role.is_some_and(|role| memory::allowed(role, memory::Access::Propose, scope)) {
        app.session_log(
            id,
            "warn",
            format!(
                "memory_read_only: refused a {scope} memory proposal from {}: only the orchestrator proposes shared memory",
                origin.unwrap_or("an unknown origin")
            ),
        )
        .await;
        return;
    }
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
    let Some(kind) = memory::parse_kind(kind) else {
        app.session_log(
            id,
            "error",
            format!("rejected a memory proposal: kind must be one of {}", memory::KINDS.join(", ")),
        )
        .await;
        return;
    };
    // Out of range or not a number reads as no confidence at all, which never promotes.
    let confidence = confidence.filter(|c| c.is_finite()).map(|c| c.clamp(0.0, 1.0));
    let key = match scope {
        "org" => s.org.clone(),
        "repo" => s.repo.clone(),
        _ => String::new(),
    };
    // Who proposed, shown in the review queue and kept as the note's provenance (issue #766): the
    // session id is the colony id, the commit is read host-side, and `origin` is always
    // `orchestrator` here — everything else was refused above (absent: that same legacy case).
    let commit = memory::colony_commit(app, &s).await;
    let source = json!({"session_id": s.id, "repo": s.repo, "commit": commit, "origin": origin.unwrap_or("orchestrator")});
    let mut note = match memory::draft(scope, &key, title, content, tags, source) {
        Ok(note) => note,
        Err(e) => {
            app.session_log(id, "error", format!("rejected a memory proposal: {e:#}"))
                .await;
            return;
        }
    };
    note.kind = kind.to_string();
    note.confidence = confidence;
    if scope == "global" {
        global_sighting(app, id, &s, note, commit).await;
        return;
    }
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

/// A colony's global proposal (issue #766): recorded as a sighting of its fleet-wide candidate, and
/// queued for review as a global note only once the candidate clears the bar. Fleet-wide memory is
/// always reviewed, whatever `require_review` says: it reaches every colony (issue #376).
async fn global_sighting(app: &Shared, id: &str, s: &Session, note: memory::Note, commit: Option<String>) {
    let sighting = memory::Sighting {
        colony: s.id.clone(),
        repo: s.repo.clone(),
        commit,
        confidence: note.confidence.unwrap_or(0.0),
        seen_at: Utc::now(),
    };
    let title = note.title.clone();
    match app.memory.record_sighting(&note, sighting).await {
        Ok(Some(global)) => match app.memory.add_proposal(global).await {
            Ok(proposal) => {
                app.session_log(
                    id,
                    "info",
                    format!(
                        "memory: \"{title}\" was seen in {} repositories with enough confidence and is waiting for your review as fleet-wide memory",
                        memory::PROMOTE_MIN_REPOS
                    ),
                )
                .await;
                let rt = app.runtimes.lock().await.get(id).cloned();
                if let Some(rt) = rt {
                    rt.broadcast(None, json!({"type": "memory_proposed", "proposal": proposal}).to_string());
                }
            }
            Err(e) => {
                app.session_log(id, "error", format!("could not queue a fleet-wide memory note: {e:#}"))
                    .await
            }
        },
        Ok(None) => {
            app.session_log(
                id,
                "info",
                format!(
                    "memory: \"{title}\" is held as a fleet-wide candidate; it is reviewed for global memory once colonies in {} repositories propose it with confidence of at least {}",
                    memory::PROMOTE_MIN_REPOS,
                    memory::PROMOTE_CONFIDENCE
                ),
            )
            .await
        }
        Err(e) => {
            app.session_log(id, "error", format!("could not record a fleet-wide memory candidate: {e:#}"))
                .await
        }
    }
}

/// Issue #84: what the colony's log says when the write kill-switch holds an effect back.
pub(crate) const AUTOPILOT_BLOCKED: &str = "autopilot: not publishing, external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS); press Create PR when \
     writes are enabled";
const FINDING_BLOCKED: &str =
    "ignored a finding: external writes are blocked (COLONIZER_NO_EXTERNAL_EFFECTS), so no issue is filed";

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
    if crate::authority::external_writes_blocked() {
        app.session_log(&id, "info", FINDING_BLOCKED.into()).await;
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
    if findings::count_in(&findings::ledger(&app, &id).await) >= findings::MAX_PER_COLONY {
        let message = format!(
            "did not file \"{}\": this colony has already filed {} findings, the most one colony may",
            finding.title,
            findings::MAX_PER_COLONY
        );
        app.session_log(&id, "warn", message).await;
        return;
    }
    // Issue #98: the independent validation is the approval. A FileIssue grant is minted over the
    // exact bytes that would be filed — reviewer the validator, builder the colony — and
    // `findings::file` re-checks it against what it renders before any gh call.
    let grant = {
        let co_author = crate::config::FileConfig::load(&app.cfg.config_dir).publish.co_author;
        let candidate = findings::candidate_hash(&finding, &s, co_author.as_ref());
        crate::authority::Grant::mint(
            &s.id,
            &finding.title,
            vec![crate::authority::Effect::FileIssue],
            candidate,
            "finding-validator",
            &s.id,
            crate::authority::GRANT_TTL_SECS,
        )
    };
    // #761: the finding reached here redacted; one that carried a secret is said out loud.
    let text = format!("{}\n{}\n{}", finding.title, finding.body, finding.evidence);
    if let Some(note) = crate::redact::redaction_note("finding-body.md", &text, "filing") {
        app.session_log(&id, "warn", note).await;
    }
    // The body goes to `gh --body-file` by path, so it is written into the local working copy.
    let outcome = findings::file(&app, &s, &finding, &app.session_dir(&id).join("finding-body.md"), &grant).await;
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
        // Through the store, so the ledger line lands in `sessions/<id>/findings.jsonl` by the same
        // name a remote backend would answer by; the store adds the newline, as `append_line` did,
        // and redacts the line (#761, `store::ledger_line`).
        let text = entry.to_string();
        let recorded = app
            .store()
            .append(&id, "findings.jsonl", text.as_bytes())
            .await
            .map_err(anyhow::Error::from);
        if let Err(e) = recorded {
            // The finding was still filed on GitHub (that happened above); what failed is the
            // colony's own record of it, so say so instead of letting the gap pass silently.
            app.storage_failed("append to the colony's findings log", &e).await;
            app.session_log(&id, "error", format!("could not record the finding in findings.jsonl: {e:#}"))
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
mod tests;
