//! Stuck-colony diagnosis (issue #230): `GET /api/sessions/{id}` carries the `events.jsonl`
//! tail as one-line digests plus a best-guess diagnosis; `GET /api/status` flags a host-wide stall.

use crate::store::SessionStore;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::{
    Shared, provider_quota,
    sessions::{Session, SessionStatus},
};

/// How much of `events.jsonl` one colony reads: the last of these bytes, never the whole file.
const TAIL_BYTES: u64 = 64 * 1024;
const MAX_EVENTS: usize = 20;
/// A digest past this many chars is cut here plus `…`.
const MAX_SUMMARY: usize = 200;
/// A `running` colony this quiet still counts as working: the watchdog's `stall_minutes` default.
pub const STALL_SECS: i64 = 15 * 60;
/// Live colonies, a waiting queue, and nothing emitting for this long: a host-wide stall.
pub const HOST_STALL_SECS: i64 = 10 * 60;

/// One tail event digested: `seq`/`ts` are whatever the line carried, `summary` the one-liner.
#[derive(Clone, Debug, Serialize)]
pub struct RecentEvent {
    pub seq: u64,
    pub ts: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub summary: String,
}

/// Exactly one of `queued`, `booting`, `working`, `waiting_on_human`, `waiting_on_provider`,
/// `stuck`. `resets_at` is the provider's reset words verbatim, only on a quota hit naming one.
#[derive(Clone, Debug, Serialize)]
pub struct Diagnosis {
    pub state: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<String>,
}

/// The host-wide stall: `null` normally, else every live colony event-quiet for `idle_secs`.
#[derive(Clone, Debug, Serialize)]
pub struct HostStall {
    pub idle_secs: i64,
    pub last_event_at: Option<String>,
    pub live: usize,
    pub queued: usize,
}

/// `GET /api/sessions/{id}`: the session plus digests and diagnosis, each omitted when absent.
/// The list route and the WS session frame stay the bare `Session`.
#[derive(Serialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub session: Session,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_events: Option<Vec<RecentEvent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnosis: Option<Diagnosis>,
}

fn state(name: &str, text: String, resets_at: Option<String>) -> Diagnosis {
    Diagnosis {
        state: name.into(),
        text,
        resets_at,
    }
}

/// Any text as one line: controls become spaces, whitespace runs collapse to one.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One line, controls and whitespace collapsed, cut at [`MAX_SUMMARY`] plus `…`. Shared with the
/// resume brief's subagent report (boot.rs, issue #756), so both shorten the same way.
pub(crate) fn shorten(text: &str) -> String {
    let line = one_line(text);
    if line.chars().count() > MAX_SUMMARY {
        format!("{}…", line.chars().take(MAX_SUMMARY).collect::<String>())
    } else {
        line
    }
}

/// The pending question's text, if the event names any.
fn question_text(event: &Value) -> Option<String> {
    let parts: Vec<String> = event
        .get("questions")?
        .as_array()?
        .iter()
        .filter_map(|q| {
            q.get("question")
                .or_else(|| q.get("header"))
                .or_else(|| q.get("text"))?
                .as_str()
        })
        .map(one_line)
        .filter(|s| !s.is_empty())
        .collect();
    (!parts.is_empty()).then(|| shorten(&parts.join("; ")))
}

/// One tail line digested, or `None` for `assistant_text_delta`/`thinking` noise and lines with
/// no wire type. Unknown types fall back to their first text-like field, else their type.
pub fn summarize(event: &Value) -> Option<RecentEvent> {
    let kind = event.get("type")?.as_str()?;
    if matches!(kind, "assistant_text_delta" | "thinking") {
        return None;
    }
    let field = |key: &str| event.get(key).and_then(Value::as_str).unwrap_or_default();
    let summary = match kind {
        "assistant_text" | "user_message" => shorten(field("text")),
        "status" => shorten(field("state")),
        "tool_call" => shorten(field("name")),
        "tool_result" => shorten(field("output")),
        "question" => question_text(event).unwrap_or_else(|| kind.to_string()),
        "turn_end" => match field("result") {
            "" => "ok".to_string(),
            result => shorten(result),
        },
        _ => ["text", "message", "state", "name", "result", "title"]
            .iter()
            .map(|key| field(key))
            .find(|s| !s.is_empty())
            .map(shorten)
            .unwrap_or_else(|| kind.to_string()),
    };
    Some(RecentEvent {
        seq: event.get("seq").and_then(Value::as_u64).unwrap_or(0),
        ts: event.get("ts").and_then(Value::as_str).map(str::to_string),
        kind: kind.to_string(),
        summary,
    })
}

/// The last lines of a colony's `events.jsonl`: at most the last 64 KiB, read through the session
/// store, whole lines only.
pub(crate) async fn tail_events(store: &dyn SessionStore, id: &str) -> Vec<Value> {
    tail_events_within(store, id, "events.jsonl", TAIL_BYTES).await
}

/// [`tail_events`] for any of a colony's event logs (`events-N.jsonl` too), with its own byte
/// budget: the store's [`SessionStore::read_tail`] — the last `bytes` of the file, whole lines
/// only — parsed and redacted. An absent or unreadable log is no events.
pub(crate) async fn tail_events_within(store: &dyn SessionStore, id: &str, name: &str, bytes: u64) -> Vec<Value> {
    let Ok(Some(tail)) = store.read_tail(id, name, bytes).await else {
        return Vec::new();
    };
    // #761: a rotated `events-N.jsonl` written before redaction existed can still carry a secret,
    // and this tail feeds the cockpit's diagnosis and a resumed colony's prompt, so every event is
    // redacted field by field on its way out. A line written since is already clean and unchanged.
    String::from_utf8_lossy(&tail)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|mut event| {
            crate::redact::redact_value(&mut event);
            event
        })
        .collect()
}

/// The last [`MAX_EVENTS`] digests, oldest first; `None` when the tail digests to nothing.
pub fn recent_events(tail: &[Value]) -> Option<Vec<RecentEvent>> {
    let mut out: Vec<RecentEvent> = tail.iter().filter_map(summarize).collect();
    if out.len() > MAX_EVENTS {
        out.drain(..out.len() - MAX_EVENTS);
    }
    (!out.is_empty()).then_some(out)
}

/// A compact human duration: `45s`, `12m`, `3h 5m`.
fn fmt_dur(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        let (hours, mins) = (secs / 3600, secs % 3600 / 60);
        if mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {mins}m")
        }
    } else {
        let (days, hours) = (secs / 86400, secs % 86400 / 3600);
        if hours == 0 {
            format!("{days}d")
        } else {
            format!("{days}d {hours}h")
        }
    }
}

/// The best guess for one session over its tail, at `now`: `None` for terminal sessions (and
/// `publishing`, neither live nor finished). First match wins: queued, booting, provider
/// failure, waiting on a human, working, stuck.
pub fn diagnose(session: &Session, tail: &[Value], now: DateTime<Utc>) -> Option<Diagnosis> {
    use SessionStatus::*;
    // Seconds from `then` to `now`, never negative: a backwards clock is quiet, not stuck.
    let age = |then: DateTime<Utc>| (now - then).num_seconds().max(0);
    let reason = session
        .attention
        .as_ref()
        .and_then(|a| a.get("reason"))
        .and_then(Value::as_str);
    match session.status {
        Queued => Some(state("queued", "queued, waiting for a free slot".into(), None)),
        Starting => {
            // `mark` records a phase only after it finishes, so the last one named is the last
            // DONE phase; the clock is the whole boot, and the next phase has the remainder.
            let phases: Vec<(&str, u64)> = session
                .boot_timing
                .as_ref()
                .and_then(|t| t.get("phases")?.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|p| Some((p.get("name")?.as_str()?, p.get("ms")?.as_u64().unwrap_or(0))))
                        .collect()
                })
                .unwrap_or_default();
            let start = session
                .boot_attempt_started_at
                .and_then(|unix| DateTime::from_timestamp(unix as i64, 0))
                .unwrap_or(session.created_at);
            let total = age(start);
            let text = match phases.last() {
                Some((last, _)) => {
                    let done_ms: u64 = phases.iter().map(|(_, ms)| ms).sum();
                    let running_ms = (total.max(0) as u64 * 1000).saturating_sub(done_ms);
                    format!(
                        "booting for {}; {last} done, next phase running {}",
                        fmt_dur(total),
                        fmt_dur((running_ms / 1000) as i64)
                    )
                }
                None => format!("booting for {}; first phase running", fmt_dur(total)),
            };
            Some(state("booting", text, None))
        }
        Running | WaitingForAnswer | Idle => {
            // Parked colonies never reach here: they are not live, and neither a quota park
            // (`provider_quota_exhausted`, events.rs) nor a hold-timeout park (`hold_timeout`,
            // queue.rs) is a state the diagnosis reads as working or finished.
            // Provider failure outranks the human wait: the tail's most recent `assistant_text`
            // with no `user_message` after it, when it classifies as exhaustion (its reset words
            // ride along verbatim), else the quota attention flag on its own, naming no reset.
            let mut candidate: Option<&str> = None;
            for event in tail {
                match event.get("type").and_then(Value::as_str) {
                    Some("assistant_text") => {
                        if let Some(text) = event.get("text").and_then(Value::as_str) {
                            candidate = Some(text);
                        }
                    }
                    Some("user_message") => candidate = None,
                    _ => {}
                }
            }
            if let Some(text) = candidate
                && let Some(hit) = provider_quota::classify_quota_exhaustion(0, "", text)
            {
                let text = match &hit.reset_at {
                    Some(reset) => format!("waiting on provider: quota exhausted, resets {reset}"),
                    None => "waiting on provider: quota exhausted".to_string(),
                };
                return Some(state("waiting_on_provider", text, hit.reset_at));
            }
            // A provider refusing the 1-hour cache TTL fails every turn the same way, so waiting for
            // the next message never helps: name the provider flag that fixes it (issues #305, #317).
            if let Some(text) = candidate
                && rejects_cache_ttl(text)
            {
                return Some(state(
                    "waiting_on_provider",
                    "waiting on provider: it rejects the 1-hour prompt-cache TTL (cache_control.ttl); \
                     turn on normalize_cache_ttl for this provider in Settings → Providers, then resume"
                        .into(),
                    None,
                ));
            }
            if reason == Some(provider_quota::QUOTA_EXHAUSTED_REASON) {
                return Some(state(
                    "waiting_on_provider",
                    "waiting on provider: quota exhausted".into(),
                    None,
                ));
            }
            if session.status != Running || matches!(reason, Some("waiting_for_answer" | "autopilot_held")) {
                let question = tail
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, e)| e.get("type").and_then(Value::as_str) == Some("question"))
                    .and_then(|(i, e)| {
                        // A question is pending only if nothing later in the tail answered it
                        // (issue #981). The slot pairing lives in `Runtime`, but the tail is what
                        // the diagnosis reads, and a `question` whose matching `question_answered`
                        // follows read as still open — the stale text the cockpit showed. The tail
                        // is short, so a forward scan for the id is enough; no replay needed.
                        let id = e.get("question_id").and_then(Value::as_str);
                        let answered = id.is_some_and(|id| {
                            tail[i + 1..].iter().any(|a| {
                                (matches!(
                                    a.get("type").and_then(Value::as_str),
                                    Some("question_answered" | "question_closed")
                                ) && a.get("question_id").and_then(Value::as_str) == Some(id))
                                    || (a.get("type").and_then(Value::as_str) == Some("tool_result")
                                        && a.get("tool_call_id").and_then(Value::as_str) == Some(id))
                            })
                        });
                        (!answered).then(|| question_text(e)).flatten()
                    });
                let text = match (question, reason, session.status) {
                    (Some(asked), _, _) => format!("waiting for an answer: {asked}"),
                    (None, Some("autopilot_held"), _) => "autopilot held, waiting for the next message".into(),
                    _ if session.status == WaitingForAnswer => "waiting for an answer".into(),
                    _ => "idle, waiting for the next message".into(),
                };
                return Some(state("waiting_on_human", text, None));
            }
            // The newest progress known: the runtime stamp, else the tail's newest timestamp.
            let tail_max = tail
                .iter()
                .filter_map(|e| e.get("ts").and_then(Value::as_str))
                .filter_map(|ts| DateTime::parse_from_rfc3339(ts).ok())
                .map(|ts| ts.with_timezone(&Utc))
                .max();
            let last = [session.last_activity_at, tail_max].into_iter().flatten().max();
            if last.is_some_and(|at| age(at) < STALL_SECS) {
                return Some(state(
                    "working",
                    format!("working (last activity {} ago)", fmt_dur(age(last.unwrap_or(now)))),
                    None,
                ));
            }
            let anchor = last.unwrap_or(session.updated_at);
            let tail_last = recent_events(tail).and_then(|v| v.into_iter().last());
            let text = match tail_last {
                Some(event) => format!(
                    "no activity for {}; last event: {}: {}",
                    fmt_dur(age(anchor)),
                    event.kind,
                    event.summary
                ),
                None => format!("no activity for {}; no events yet", fmt_dur(age(anchor))),
            };
            Some(state("stuck", text, None))
        }
        _ => None,
    }
}

/// Whether an assistant text is Claude Code reporting an endpoint that refuses `cache_control.ttl`,
/// e.g. `API Error: 400 `cache_control.ttl: 1h` is not supported`.
fn rejects_cache_ttl(text: &str) -> bool {
    text.starts_with("API Error") && text.contains("cache_control.ttl") && text.contains("not supported")
}

/// The host-wide stall predicate over the newest event time known: live colonies, a non-empty
/// queue, and every live colony event-quiet for [`HOST_STALL_SECS`].
pub fn host_stall(last: Option<DateTime<Utc>>, live: usize, queued: usize, now: DateTime<Utc>) -> Option<HostStall> {
    if live == 0 || queued == 0 {
        return None;
    }
    let idle_secs = (now - last?).num_seconds().max(0);
    (idle_secs >= HOST_STALL_SECS).then(|| HostStall {
        idle_secs,
        last_event_at: last.map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        live,
        queued,
    })
}

/// The newest event time across live colonies, cheaply: the runtime's activity stamp when one is
/// kept, else the `events.jsonl` mtime — metadata only, no content reads.
pub(crate) async fn status_stall(app: &Shared) -> Option<HostStall> {
    let sessions = app.sessions.read().await;
    let queued = sessions.iter().filter(|s| s.status == SessionStatus::Queued).count();
    let live: Vec<Session> = sessions.iter().filter(|s| s.status.is_live()).cloned().collect();
    drop(sessions);
    if live.is_empty() || queued == 0 {
        return None;
    }
    let mut last: Option<DateTime<Utc>> = None;
    for session in &live {
        // Clone the runtime out under its lock, then read activity once the guard is dropped.
        let runtime = app.runtimes.lock().await.get(&session.id).cloned();
        let at = match runtime {
            Some(rt) => Some(rt.activity.lock().await.last),
            None => app
                .store()
                .stat(&session.id, "events.jsonl")
                .await
                .ok()
                .flatten()
                .and_then(|stat| stat.modified)
                .map(DateTime::<Utc>::from),
        };
        last = last.into_iter().chain(at).max();
    }
    host_stall(last, live.len(), queued, Utc::now())
}

/// The `GET /api/sessions/{id}` body: the session (activity filled in by the caller) plus its
/// tail digests and diagnosis.
pub(crate) async fn for_session(app: &Shared, session: Session) -> SessionDetail {
    let tail = tail_events(app.store(), &session.id).await;
    SessionDetail {
        diagnosis: diagnose(&session, &tail, Utc::now()),
        recent_events: recent_events(&tail),
        session,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{
        self,
        tests::{app_with_colony, colony},
    };
    use axum::{
        Json,
        extract::{Path, State},
    };

    const QUOTA_TEXT: &str = "API Error: Server is temporarily limiting requests (not your usage limit) · \
        Your token-plan 1-week quota has been exhausted. The quota will reset at 09-23 07:54:00 UTC.";

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-23T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn with(status: SessionStatus, f: impl FnOnce(&mut Session)) -> Session {
        let mut s = colony("acme", status);
        f(&mut s);
        s
    }

    /// The incident: `idle` + `autopilot_held`, whose only symptom is the quota `assistant_text`
    /// in the tail. The real `get` handler must read it as waiting on the provider, with the
    /// upstream's reset words verbatim.
    #[tokio::test]
    async fn the_quota_incident_diagnoses_waiting_on_provider() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        app.update_session("abc", |s| {
            s.attention = Some(serde_json::json!({"reason": "autopilot_held", "since": now(), "nudges": 0}));
            s.error = None;
        })
        .await;
        let tail = format!(
            "{{\"seq\":41,\"ts\":\"2026-09-23T07:50:00Z\",\"type\":\"assistant_text\",\"text\":\"{QUOTA_TEXT}\"}}\n\
             {{\"seq\":42,\"ts\":\"2026-09-23T07:50:01Z\",\"type\":\"turn_end\",\"is_error\":true,\"result\":\"{QUOTA_TEXT}\"}}\n\
             {{\"seq\":43,\"ts\":\"2026-09-23T07:50:01Z\",\"type\":\"status\",\"state\":\"idle\"}}\n"
        );
        tokio::fs::write(app.session_dir("abc").join("events.jsonl"), &tail)
            .await
            .unwrap();
        let Json(detail) = sessions::get(State(app), Path("abc".to_string())).await.unwrap();
        let wire = serde_json::to_value(&detail).unwrap();
        assert_eq!(wire["diagnosis"]["state"], "waiting_on_provider");
        assert_eq!(wire["diagnosis"]["resets_at"], "09-23 07:54:00 UTC");
        assert!(
            wire["recent_events"].as_array().unwrap().iter().any(|e| {
                e["type"] == "assistant_text" && e["summary"].as_str().unwrap().contains("quota has been exhausted")
            }),
            "the tail digest carries the quota text: {wire}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn diagnose_states_in_precedence_order() {
        use SessionStatus::*;
        let held = || with(Idle, |s| s.attention = Some(serde_json::json!({"reason": "autopilot_held"})));
        let quota = || vec![serde_json::json!({"seq": 1, "type": "assistant_text", "text": QUOTA_TEXT})];
        let cache_ttl = || {
            vec![serde_json::json!({"seq": 1, "type": "assistant_text",
                "text": "API Error: 400 `cache_control.ttl: 1h` is not supported"})]
        };
        let booting = with(Starting, |s| {
            s.boot_timing = Some(serde_json::json!({"phases": [{"name": "clone", "ms": 60000}, {"name": "git", "ms": 30000}]}));
            s.created_at = now() - chrono::Duration::minutes(3);
        });
        let booting_first = with(Starting, |s| {
            s.created_at = now() - chrono::Duration::seconds(45);
        });
        let stale = with(Running, |s| {
            s.last_activity_at = Some(now() - chrono::Duration::hours(2));
            s.updated_at = now() - chrono::Duration::hours(2);
        });
        let asking =
            vec![serde_json::json!({"seq": 2, "type": "question", "question_id": "q", "questions": [{"question": "Ship it?"}]})];
        // A quota tail outranks the human wait an idle colony otherwise reads as; a stale run
        // names its last event while noise never digests; finished colonies carry nothing.
        let noisy: Vec<Value> = [
            serde_json::json!({"type": "thinking"}),
            serde_json::json!({"type": "assistant_text_delta"}),
        ]
        .into_iter()
        .chain((0..30).map(|i| serde_json::json!({"seq": i, "type": "status", "state": "working"})))
        .chain(std::iter::once(
            serde_json::json!({"seq": 99, "type": "tool_call", "name": "Bash"}),
        ))
        .collect();
        // (session, tail, want state, text must contain every needle, want resets_at).
        type Case = (
            Session,
            Vec<Value>,
            Option<&'static str>,
            &'static [&'static str],
            Option<&'static str>,
        );
        let cases: Vec<Case> = vec![
            (
                booting,
                vec![],
                Some("booting"),
                &["booting for 3m", "git done", "next phase running 1m"],
                None,
            ),
            (
                booting_first,
                vec![],
                Some("booting"),
                &["booting for 45s", "first phase running"],
                None,
            ),
            (with(Queued, |_| {}), vec![], Some("queued"), &[], None),
            (
                with(Running, |s| s.last_activity_at = Some(now() - chrono::Duration::seconds(60))),
                vec![],
                Some("working"),
                &["1m ago"],
                None,
            ),
            (
                held(),
                quota(),
                Some("waiting_on_provider"),
                &["quota exhausted"],
                Some("09-23 07:54:00 UTC"),
            ),
            (held(), asking, Some("waiting_on_human"), &["Ship it?"], None),
            (
                held(),
                cache_ttl(),
                Some("waiting_on_provider"),
                &["cache_control.ttl", "normalize_cache_ttl"],
                None,
            ),
            (
                held(),
                cache_ttl()
                    .into_iter()
                    .chain(std::iter::once(
                        serde_json::json!({"seq": 2, "type": "user_message", "text": "fix"}),
                    ))
                    .collect(),
                Some("waiting_on_human"),
                &["autopilot held"],
                None,
            ),
            (stale, noisy, Some("stuck"), &["2h", "tool_call: Bash"], None),
            (with(Stopped, |_| {}), quota(), None, &[], None),
            (with(Publishing, |_| {}), quota(), None, &[], None),
        ];
        for (i, (session, tail, want, needles, resets)) in cases.into_iter().enumerate() {
            match (diagnose(&session, &tail, now()), want) {
                (None, None) => {}
                (Some(got), Some(state)) => {
                    assert_eq!(got.state, state, "case {i}");
                    for needle in needles {
                        assert!(got.text.contains(needle), "case {i}: {} misses {needle}", got.text);
                    }
                    assert_eq!(got.resets_at.as_deref(), resets, "case {i}");
                }
                (got, want) => panic!("case {i}: got {got:?}, want {want:?}"),
            }
        }
        // The digests behind the table: noise skipped, capped at twenty, oldest first.
        let tail: Vec<Value> = (0..30)
            .map(|i| serde_json::json!({"seq": i, "type": "status", "state": "working"}))
            .collect();
        let recent = recent_events(&tail).unwrap();
        assert_eq!((recent.len(), recent[0].seq), (20, 10));
        assert!(recent_events(&[]).is_none());
    }

    /// Issue #981: a `question` whose matching `question_answered` follows it in the tail is not
    /// pending, so the diagnosis must not keep naming its stale text — the cockpit read that as a
    /// question still waiting on the person.
    #[test]
    fn an_answered_question_is_not_diagnosed_as_pending() {
        let session = with(SessionStatus::Idle, |s| {
            s.attention = Some(serde_json::json!({"reason": "autopilot_held"}));
        });
        let answered = vec![
            serde_json::json!({"seq": 1, "type": "question", "question_id": "q1", "questions": [{"question": "Ship it?"}]}),
            serde_json::json!({"seq": 2, "type": "question_answered", "question_id": "q1", "answers": {}}),
        ];
        let got = diagnose(&session, &answered, now()).unwrap();
        assert!(
            !got.text.contains("Ship it?"),
            "the answered question is not pending: {}",
            got.text
        );
        assert!(
            got.text.contains("autopilot held"),
            "it falls through to the held line: {}",
            got.text
        );

        // An answer to a different id leaves this question pending, text and all.
        let other = vec![
            serde_json::json!({"seq": 1, "type": "question", "question_id": "q1", "questions": [{"question": "Ship it?"}]}),
            serde_json::json!({"seq": 2, "type": "question_answered", "question_id": "q2", "answers": {}}),
        ];
        let got = diagnose(&session, &other, now()).unwrap();
        assert!(
            got.text.contains("Ship it?"),
            "an unrelated answer does not resolve it: {}",
            got.text
        );
    }

    #[test]
    fn host_stall_fires_only_when_live_idle_and_queued() {
        let idle = Some(now() - chrono::Duration::minutes(11));
        let stall = host_stall(idle, 2, 1, now()).unwrap();
        assert_eq!((stall.idle_secs, stall.live, stall.queued), (11 * 60, 2, 1));
        for (last, live, queued) in [
            (Some(now() - chrono::Duration::minutes(9)), 2, 1),
            (idle, 2, 0),
            (idle, 0, 1),
            (None, 2, 1),
        ] {
            assert!(host_stall(last, live, queued, now()).is_none(), "{last:?} {live} {queued}");
        }
    }

    /// The list route stays the bare `Session`: no tail reads, no diagnosis.
    #[tokio::test]
    async fn list_carries_no_recent_events_or_diagnosis() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        tokio::fs::write(
            app.session_dir("abc").join("events.jsonl"),
            "{\"seq\":1,\"type\":\"status\",\"state\":\"idle\"}\n",
        )
        .await
        .unwrap();
        let Json(list) = sessions::list_bare(State(app), None).await;
        let wire = serde_json::to_value(&list).unwrap();
        assert!(wire[0].get("recent_events").is_none());
        assert!(wire[0].get("diagnosis").is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
