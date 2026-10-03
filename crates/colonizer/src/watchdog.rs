//! Watchdog module: notices colonies that stopped making progress, nudges them, and flags them for
//! the user when nudging doesn't help. The decision is a pure function so it can be tested with a
//! fixed clock; the loop around it runs once a minute.

use crate::{
    Shared,
    orgs::effective_watchdog,
    protocol::Origin,
    sessions::{Runtime, Session, SessionStatus},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogSettings {
    pub enabled: bool,
    pub stall_minutes: u64,
    pub max_nudges: u64,
    pub waiting_minutes: u64,
}

/// Per-colony progress bookkeeping, kept in the colony's runtime.
#[derive(Clone, Debug)]
pub struct Activity {
    pub last: DateTime<Utc>,
    pub nudges: u64,
    pub last_nudge: Option<DateTime<Utc>>,
    pub question_since: Option<DateTime<Utc>>,
    /// Questions autonomous mode has answered for this colony, against its own cap.
    pub judged: u64,
    /// Judged attempts in a row that failed short of a refusal — a provider that could not be
    /// reached, an HTTP error, a reply that could not be used. A good answer or a cleared
    /// question resets it; enough in a row and the judge stops retrying, so the question reaches
    /// a person.
    pub judge_failures: u64,
    /// The question id whose above-the-ceiling note is already in the log, so the judge's
    /// once-per-question "left for you" line does not repeat every half-minute tick.
    pub risk_announced: Option<String>,
    /// Automatic recoveries the recovery point (`recovery.rs`) has chosen for this colony, against
    /// its own per-colony cap (issue #586). In memory like the rest of `Activity`: a restart resets
    /// it, which at worst allows a few more automatic recoveries than the cap names.
    pub recoveries: u32,
}

impl Activity {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            last: now,
            nudges: 0,
            last_nudge: None,
            question_since: None,
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    Working,
    WaitingForAnswer,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Nothing,
    Nudge,
    Flag(&'static str),
    Clear,
}

/// Attention reasons the watchdog sets and may clear; others (such as `autopilot_held`) belong to their setter.
/// Shared with usage.rs, which buckets them as its closed failure labels.
pub(crate) const WATCHDOG_REASONS: [&str; 3] = ["stalled", "waiting_for_answer", "nudges_exhausted"];

pub fn decide(
    settings: &WatchdogSettings,
    now: DateTime<Utc>,
    state: Observed,
    activity: &Activity,
    attention: Option<&str>,
) -> Decision {
    if !settings.enabled {
        let ours = attention.is_some_and(|reason| WATCHDOG_REASONS.contains(&reason));
        return if ours { Decision::Clear } else { Decision::Nothing };
    }
    match state {
        Observed::WaitingForAnswer => {
            let waited = activity
                .question_since
                .map(|since| now - since)
                .unwrap_or_else(Duration::zero);
            if waited >= Duration::minutes(settings.waiting_minutes as i64) && attention != Some("waiting_for_answer") {
                Decision::Flag("waiting_for_answer")
            } else {
                Decision::Nothing
            }
        }
        Observed::Working => {
            let reference = activity.last_nudge.map_or(activity.last, |nudge| nudge.max(activity.last));
            if now - reference < Duration::minutes(settings.stall_minutes as i64) {
                Decision::Nothing
            } else if activity.nudges < settings.max_nudges {
                Decision::Nudge
            } else if attention != Some("nudges_exhausted") {
                Decision::Flag("nudges_exhausted")
            } else {
                Decision::Nothing
            }
        }
        Observed::Other => {
            if attention == Some("waiting_for_answer") {
                Decision::Clear
            } else {
                Decision::Nothing
            }
        }
    }
}

pub fn nudge_text(minutes: u64) -> String {
    let span = if minutes == 1 {
        "1 minute".to_string()
    } else {
        format!("{minutes} minutes")
    };
    format!(
        "Watchdog check: this colony has shown no progress for {span}. If a command or process is hanging, \
         stop it and try another way. If you need a decision from the maintainer, ask with a choice card. Otherwise, \
         continue the task and report what you're doing."
    )
}

/// How long a final `assistant_text` may stand with no tool call in flight, no open question and no
/// gateway request before the watchdog treats the still-open turn as wedged (issue #878). Two
/// minutes against the 60 s tick: a runner that is only thinking between blocks is left alone, and a
/// turn the runner will never end is finished within three.
const TURN_END_GRACE: Duration = Duration::seconds(120);

/// Whether a colony's quiet final answer is a turn the watchdog should finish (issue #878): the
/// runner said it was done, its final text is past the grace, and nothing is in flight — no tool
/// call, no open question, no request through the gateway. Pure, so every condition is pinned with a
/// fixed clock.
fn turn_end_due(
    now: DateTime<Utc>,
    final_text_at: Option<DateTime<Utc>>,
    open_tool_calls: usize,
    open_question: bool,
    gateway_busy: bool,
) -> bool {
    let Some(final_at) = final_text_at else { return false };
    now - final_at >= TURN_END_GRACE && open_tool_calls == 0 && !open_question && !gateway_busy
}

/// Whether agentd's `/v1/health` body says its runner is running. A body that cannot be read is not
/// a "yes": the finish runs only on an unambiguous liveness signal.
fn runner_running(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["agent"]["running"].as_bool())
        .unwrap_or(false)
}

/// Finishes a colony's turn the runner said was over but never ended (issue #878). The runner emits
/// its final `assistant_text` and then stops before `turn_end` — a wedge its own event stream can
/// never report — so the watchdog detects it and, when agentd still answers with its runner running,
/// synthesises the end: a `watchdog_turn_end` line goes on the record and the ordinary turn-end path
/// runs, so verification and publish proceed.
///
/// The probe is a liveness check only — that agentd and the runner process are up — not a reading of
/// the turn's state: agentd's `running` is process liveness, and the claude-code runner emits
/// `turn_end` only as it goes idle, so a wedged turn reads `working` either way. Whether the turn is
/// done is decided on the mothership side, by [`turn_end_due`]: no tool call in flight, no open
/// question, nothing through the gateway.
///
/// The claim is consumed only on the success path: a probe that times out leaves the final answer
/// standing, so the next tick retries rather than losing the recovery. The success path re-reads the
/// conditions after the probe — the probe can take ten seconds, and an agent that starts working in
/// that window is not interrupted. A later real `turn_end` is harmless — `pr_mark` (`events.rs`)
/// does not publish over an unchanged description, and a synthetic end carries no spend to
/// double-count.
async fn maybe_finish_turn(app: &Shared, s: &Session, rt: &Arc<Runtime>, settings: &WatchdogSettings, now: DateTime<Utc>) {
    if !settings.enabled || s.status != SessionStatus::Running {
        return;
    }
    let final_text_at = *rt.final_text_at.lock().await;
    let open_tool_calls = rt.open_tool_calls.lock().await.len();
    let open_question = rt.open_question.lock().await.is_some();
    let busy = app.gateway.colony_busy(&s.id);
    if !turn_end_due(now, final_text_at, open_tool_calls, open_question, busy) {
        return;
    }
    let final_at = final_text_at.expect("turn_end_due is true only with a final timestamp");
    match crate::sessions::agentd_http(app, s, "GET", "/v1/health").await {
        Ok((200, body)) if runner_running(&body) => {
            // The probe can take up to ten seconds, and the runner may have started working again in
            // that window — a tool call, a delta, a fresh question — or a request may have arrived
            // through the gateway. A busy agent is not interrupted: re-read the conditions now and
            // claim the end only if the final answer is the same one and the turn still reads quiet,
            // the compare and the clear under the one lock so a racing event cannot slip between them.
            let open_tool_calls = rt.open_tool_calls.lock().await.len();
            let open_question = rt.open_question.lock().await.is_some();
            let busy = app.gateway.colony_busy(&s.id);
            {
                let mut claim = rt.final_text_at.lock().await;
                if !(*claim == Some(final_at) && turn_end_due(now, *claim, open_tool_calls, open_question, busy)) {
                    return;
                }
                *claim = None;
            }
            crate::validation::emit_chain(
                app,
                &s.id,
                json!({"type": "watchdog_turn_end", "after_secs": (now - final_at).num_seconds()}),
            )
            .await;
            app.session_log_as(
                Origin::Watchdog,
                &s.id,
                "warn",
                "watchdog: the agent said it was done but its turn never ended; finishing the turn so the work can publish"
                    .into(),
            )
            .await;
            crate::usage::note_watchdog_turn_end();
            crate::events::finish_turn(app, &s.id, rt, false, None, None, None).await;
        }
        // agentd is up but its runner is not running: the status path already owns that state (an
        // `exited`/`error` runner is held or parked there), so this leaves it alone and consumes the
        // claim without a log.
        Ok((200, _)) => *rt.final_text_at.lock().await = None,
        // agentd itself is not answering, so its runner cannot be restarted through it. Leave the
        // claim standing: a transient probe failure must not lose the recovery, so the next tick
        // retries. Record it once per final answer, not every tick, and let the existing stall
        // handling (a nudge, then the flag) take over in the meantime.
        _ => {
            let first = {
                let mut logged = rt.final_text_logged.lock().await;
                let first = *logged != Some(final_at);
                *logged = Some(final_at);
                first
            };
            if first {
                app.session_log_as(
                    Origin::Watchdog,
                    &s.id,
                    "error",
                    "watchdog: the agent's turn never ended and agentd is not answering; leaving this to the stall handling"
                        .into(),
                )
                .await;
            }
        }
    }
}

/// Runs the watchdog forever, checking every live colony once a minute.
pub async fn run(app: Shared) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        check_all(&app).await;
    }
}

async fn check_all(app: &Shared) {
    // A colony blocked on an exhausted provider is flagged with the quota reason and left out of the
    // nudging below (issue #760): nudging it only sends another request the plan cannot answer.
    // `starting` colonies are included there — one whose agent never got a turn out would otherwise
    // sit in `starting` with no flag at all.
    let blocked = crate::quota_cards::flag_blocked(app).await;
    let sessions = app.sessions.read().await.clone();
    let modules = app.modules.read().await.clone();
    let now = Utc::now();
    // A suspended colony is skipped (issue #562): its microVM was removed on purpose and its link
    // with it, so there is nothing to nudge and the question is already a person's to answer — the
    // restore pass, not a nudge, brings it back.
    for s in sessions.into_iter().filter(|s| {
        s.status.is_live() && s.status != SessionStatus::Starting && s.suspended.is_none() && !blocked.contains(&s.id)
    }) {
        let Some(rt) = app.runtimes.lock().await.get(&s.id).cloned() else {
            continue;
        };
        let settings = effective_watchdog(&modules, &app.org_settings(&s.org));
        // A turn the runner said was over but never ended is finished first (issue #878): the agent
        // is done, so it must not also be nudged as though it had stalled.
        maybe_finish_turn(app, &s, &rt, &settings, now).await;
        let state = match s.status {
            SessionStatus::Running => Observed::Working,
            SessionStatus::WaitingForAnswer => Observed::WaitingForAnswer,
            _ => Observed::Other,
        };
        let attention = s.attention.as_ref().and_then(|a| a["reason"].as_str()).map(String::from);
        // A request waiting on a slow model through the gateway is progress, not a stall.
        if app.gateway.colony_busy(&s.id) {
            {
                let mut current = rt.activity.lock().await;
                current.last = now;
                current.nudges = 0;
                current.last_nudge = None;
            }
            // Gateway traffic alone does not lift the watchdog's final flag (issue #760): an agent
            // retrying into a failing provider keeps the gateway busy without making progress, and
            // clearing the flag here left "this colony needs you" in the log with nothing in the
            // cockpit. Only a real agent event (events.rs) clears `nudges_exhausted`.
            if attention
                .as_deref()
                .is_some_and(|reason| reason != "waiting_for_answer" && reason != "nudges_exhausted")
            {
                app.update_session(&s.id, |x| x.attention = None).await;
                continue;
            }
            if attention.as_deref() == Some("nudges_exhausted") {
                continue;
            }
        }
        let activity = rt.activity.lock().await.clone();
        match decide(&settings, now, state, &activity, attention.as_deref()) {
            Decision::Nothing => {}
            Decision::Nudge => {
                // The recovery point (issue #586): nudging is the rule, and when the point is on Jev
                // may answer with another option. Off leaves this branch exactly as it was.
                let (action, did) = crate::recovery::handle(app, &s, crate::recovery::Failure::Stall, "nudge_agent", false).await;
                let nudges = activity.nudges + 1;
                {
                    let mut current = rt.activity.lock().await;
                    current.nudges = nudges;
                    current.last_nudge = Some(now);
                }
                match action {
                    // `ask_human` and `stop` hand the colony to a person: no message to the agent,
                    // the "needs you" flag instead. `stop` also interrupts the turn through the
                    // existing, non-destructive interrupt, so nothing further is pushed.
                    crate::recovery::Action::AskHuman | crate::recovery::Action::Stop => {
                        if action == crate::recovery::Action::Stop {
                            rt.send_command(json!({"type": "interrupt"}));
                            rt.interrupted.store(true, Ordering::SeqCst);
                        }
                        // The `stalled` flag the nudge sets, so the colony reads as needing you the
                        // same way; the log says whether Jev chose it or the cap forced it.
                        let why = if did == "cap" {
                            "the recovery cap was reached".to_string()
                        } else {
                            format!("Jev chose {}", action.as_str())
                        };
                        flag(
                            app,
                            &s.id,
                            "stalled",
                            activity.last,
                            nudges,
                            format!(
                                "watchdog: {why} after {} min of no progress; this colony needs you",
                                settings.stall_minutes
                            ),
                        )
                        .await;
                    }
                    other => {
                        // The nudge is the watchdog's own message, with the stall span in it; an
                        // option with no message of its own falls back to it, as the rule would.
                        let text = match other.agent_message() {
                            Some(text) => text.to_string(),
                            None => nudge_text(settings.stall_minutes),
                        };
                        crate::recovery::send_user_message(&rt, "watchdog", &text);
                        // A nudge keeps the log line it has always had; any other pick says which.
                        let message = if other == crate::recovery::Action::NudgeAgent {
                            format!(
                                "watchdog: no progress for {} min, nudged the agent ({nudges}/{})",
                                settings.stall_minutes, settings.max_nudges
                            )
                        } else {
                            format!(
                                "watchdog: no progress for {} min, sent the agent the {} recovery ({nudges}/{})",
                                settings.stall_minutes,
                                other.as_str(),
                                settings.max_nudges
                            )
                        };
                        app.session_log_as(Origin::Watchdog, &s.id, "info", message).await;
                        app.update_session(&s.id, |x| {
                            x.attention = Some(json!({"reason": "stalled", "since": activity.last, "nudges": nudges}));
                        })
                        .await;
                    }
                }
            }
            Decision::Flag(reason) => {
                let message = match reason {
                    "waiting_for_answer" => format!(
                        "watchdog: a question has been waiting for over {} min",
                        settings.waiting_minutes
                    ),
                    _ => format!(
                        "watchdog: still no progress after {} nudges; this colony needs you",
                        activity.nudges
                    ),
                };
                let since = if reason == "waiting_for_answer" {
                    activity.question_since.unwrap_or(now)
                } else {
                    activity.last
                };
                flag(app, &s.id, reason, since, activity.nudges, message).await;
            }
            Decision::Clear => {
                app.update_session(&s.id, |x| x.attention = None).await;
            }
        }
    }
}

/// Raises the watchdog's flag on a colony: the attention flag the cockpit's "needs you" list and
/// notifications read, and the log line saying why, together (issue #760: the final "needs you"
/// must reach the cockpit, not only the log). The flag is written first, so a colony whose log
/// says it needs you always carries the flag that shows it.
async fn flag(app: &Shared, id: &str, reason: &str, since: DateTime<Utc>, nudges: u64, message: String) {
    app.update_session(id, |x| {
        x.attention = Some(json!({"reason": reason, "since": since, "nudges": nudges}));
    })
    .await;
    app.session_log_as(Origin::Watchdog, id, "error", message).await;
}

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    tokio::spawn(run(app.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: WatchdogSettings = WatchdogSettings {
        enabled: true,
        stall_minutes: 15,
        max_nudges: 2,
        waiting_minutes: 30,
    };

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_000_000, 0).unwrap() + Duration::minutes(minutes)
    }

    #[test]
    fn working_colonies_are_nudged_once_per_interval_then_flagged() {
        let mut activity = Activity::new(at(0));
        assert_eq!(
            decide(&SETTINGS, at(14), Observed::Working, &activity, None),
            Decision::Nothing
        );
        assert_eq!(decide(&SETTINGS, at(15), Observed::Working, &activity, None), Decision::Nudge);

        activity.nudges = 1;
        activity.last_nudge = Some(at(15));
        assert_eq!(
            decide(&SETTINGS, at(20), Observed::Working, &activity, Some("stalled")),
            Decision::Nothing
        );
        assert_eq!(
            decide(&SETTINGS, at(30), Observed::Working, &activity, Some("stalled")),
            Decision::Nudge
        );

        activity.nudges = 2;
        activity.last_nudge = Some(at(30));
        assert_eq!(
            decide(&SETTINGS, at(44), Observed::Working, &activity, Some("stalled")),
            Decision::Nothing
        );
        assert_eq!(
            decide(&SETTINGS, at(45), Observed::Working, &activity, Some("stalled")),
            Decision::Flag("nudges_exhausted")
        );
        assert_eq!(
            decide(&SETTINGS, at(90), Observed::Working, &activity, Some("nudges_exhausted")),
            Decision::Nothing
        );
    }

    #[test]
    fn progress_after_a_nudge_restarts_the_clock() {
        let activity = Activity {
            last: at(20),
            nudges: 1,
            last_nudge: Some(at(15)),
            question_since: None,
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
        };
        assert_eq!(
            decide(&SETTINGS, at(34), Observed::Working, &activity, None),
            Decision::Nothing
        );
        assert_eq!(decide(&SETTINGS, at(35), Observed::Working, &activity, None), Decision::Nudge);
    }

    #[test]
    fn unanswered_questions_are_flagged_not_nudged() {
        let activity = Activity {
            last: at(0),
            nudges: 0,
            last_nudge: None,
            question_since: Some(at(0)),
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
        };
        assert_eq!(
            decide(&SETTINGS, at(29), Observed::WaitingForAnswer, &activity, None),
            Decision::Nothing
        );
        assert_eq!(
            decide(&SETTINGS, at(30), Observed::WaitingForAnswer, &activity, None),
            Decision::Flag("waiting_for_answer")
        );
        assert_eq!(
            decide(
                &SETTINGS,
                at(60),
                Observed::WaitingForAnswer,
                &activity,
                Some("waiting_for_answer")
            ),
            Decision::Nothing
        );
        assert_eq!(
            decide(&SETTINGS, at(61), Observed::Other, &activity, Some("waiting_for_answer")),
            Decision::Clear
        );
    }

    /// A colony with a stalled runtime and its nudges spent, under a watchdog of 15 min / 2 nudges.
    async fn stalled_app(name: &str, status: SessionStatus) -> (Shared, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-watchdog-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(
            root.join("config/orgs.json"),
            r#"{"acme": {"watchdog": {"enabled": true, "stall_minutes": 15, "max_nudges": 2}}}"#,
        )
        .unwrap();
        let app = crate::tests::test_app(&root);
        let mut s = crate::sessions::tests::colony("acme", status);
        s.id = "w1".into();
        s.allowed_providers = Some(vec!["bailian".into()]);
        app.sessions.write().await.push(s);
        std::fs::create_dir_all(app.session_dir("w1")).unwrap();
        let rt = app.runtime("w1").await;
        {
            let mut activity = rt.activity.lock().await;
            activity.last = Utc::now() - Duration::hours(2);
            activity.nudges = 2;
            activity.last_nudge = Some(Utc::now() - Duration::hours(1));
        }
        (app, root)
    }

    /// The final "this colony needs you" raises the attention flag the cockpit reads, not only a
    /// log line (issue #760), and gateway traffic alone does not take it down again.
    #[tokio::test]
    async fn the_final_needs_you_sets_the_attention_flag() {
        let (app, root) = stalled_app("flag", SessionStatus::Running).await;
        check_all(&app).await;
        let attention = app.session("w1").await.unwrap().attention.expect("flagged");
        assert_eq!(attention["reason"], "nudges_exhausted");
        assert_eq!(attention["nudges"], 2);
        let logs = app.runtime("w1").await.logs.lock().await.clone();
        assert!(
            logs.iter()
                .any(|l| l["message"].as_str().is_some_and(|m| m.contains("this colony needs you"))),
            "and the log says so: {logs:?}"
        );
        check_all(&app).await;
        assert_eq!(
            app.session("w1").await.unwrap().attention.unwrap()["reason"],
            "nudges_exhausted",
            "the flag stays up"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// With the recovery point off, a nudge is byte-for-byte the log line it always was (issue #586):
    /// the decision point must not change what a colony sees when it is not switched on.
    #[tokio::test]
    async fn an_off_recovery_point_nudges_with_the_original_log_line() {
        let (app, root) = stalled_app("nudge-line", SessionStatus::Running).await;
        {
            let rt = app.runtime("w1").await;
            let mut activity = rt.activity.lock().await;
            activity.nudges = 0;
            activity.last_nudge = None;
        }
        check_all(&app).await;
        let logs = app.runtime("w1").await.logs.lock().await.clone();
        assert!(
            logs.iter().any(|l| l["message"]
                .as_str()
                .is_some_and(|m| m.contains("no progress for 15 min, nudged the agent (1/2)"))),
            "the off-mode nudge line is unchanged: {logs:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A colony blocked on an exhausted provider gets the quota flag and is not nudged: another
    /// request cannot be answered until the plan resets (issue #760).
    #[tokio::test]
    async fn a_quota_blocked_colony_is_flagged_with_the_quota_reason_not_nudged() {
        let (app, root) = stalled_app("quota", SessionStatus::Running).await;
        app.gateway
            .mark_quota_exhausted("bailian", None, Some(Utc::now().timestamp() + 600));
        app.gateway.note_colony_quota("w1", "bailian");
        check_all(&app).await;
        let attention = app.session("w1").await.unwrap().attention.expect("flagged");
        assert_eq!(attention["reason"], crate::provider_quota::QUOTA_EXHAUSTED_REASON);
        assert_eq!(attention["provider"], "bailian");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn disabled_watchdog_clears_only_its_own_attention() {
        let settings = WatchdogSettings {
            enabled: false,
            ..SETTINGS
        };
        let activity = Activity::new(at(0));
        assert_eq!(
            decide(&settings, at(500), Observed::Working, &activity, None),
            Decision::Nothing
        );
        assert_eq!(
            decide(&settings, at(500), Observed::Working, &activity, Some("stalled")),
            Decision::Clear
        );
        assert_eq!(
            decide(&settings, at(500), Observed::Other, &activity, Some("autopilot_held")),
            Decision::Nothing
        );
        assert_eq!(
            decide(&SETTINGS, at(500), Observed::Other, &activity, Some("autopilot_held")),
            Decision::Nothing
        );
    }

    #[test]
    fn a_turn_is_finished_only_when_the_runner_is_quiet() {
        let now = at(0);
        let stale = now - Duration::seconds(120);
        assert!(turn_end_due(now, Some(stale), 0, false, false), "the grace is met");
        assert!(
            !turn_end_due(now, Some(now - Duration::seconds(119)), 0, false, false),
            "inside the grace it is left alone"
        );
        assert!(!turn_end_due(now, None, 0, false, false), "no final answer to finish");
        assert!(!turn_end_due(now, Some(stale), 1, false, false), "a tool call is in flight");
        assert!(!turn_end_due(now, Some(stale), 0, true, false), "a question is open");
        assert!(
            !turn_end_due(now, Some(stale), 0, false, true),
            "a gateway request is in flight"
        );
    }

    #[test]
    fn only_a_running_runner_is_read_as_alive() {
        assert!(runner_running(r#"{"agent":{"running":true,"state":"working"}}"#));
        assert!(!runner_running(r#"{"agent":{"running":false,"state":"idle"}}"#));
        assert!(!runner_running("{}"));
        assert!(!runner_running("not json"));
    }

    /// A colony with agentd answering `/v1/health` with `body` on a local port, its token on disk,
    /// and its stall clock quiet, so only the turn-end recovery is under test. The probe walks the
    /// real path (`sessions::agentd::agentd_http`), not a stub.
    async fn app_with_agentd(name: &str, body: &str) -> (Shared, std::path::PathBuf) {
        let (app, root) = stalled_app(name, SessionStatus::Running).await;
        {
            let rt = app.runtime("w1").await;
            let mut activity = rt.activity.lock().await;
            activity.last = Utc::now();
            activity.nudges = 0;
            activity.last_nudge = None;
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let response = response.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        let vm = app.session_dir("w1").join("vm");
        std::fs::create_dir_all(&vm).unwrap();
        std::fs::write(vm.join("token"), "test-token\n").unwrap();
        app.update_session("w1", |x| x.local_port = Some(port)).await;
        (app, root)
    }

    /// Acceptance (issue #878): a final answer the runner never ended, with agentd still answering,
    /// is finished — the `watchdog_turn_end` event is recorded and the turn-end path runs.
    #[tokio::test]
    async fn a_final_answer_the_runner_never_ended_is_finished() {
        let (app, root) = app_with_agentd("finish", r#"{"agent":{"running":true,"state":"working"}}"#).await;
        app.update_session("w1", |x| x.autopilot = true).await;
        let rt = app.runtime("w1").await;
        crate::events::handle_agent_event(
            &app,
            "w1",
            &rt,
            r#"{"seq":1,"type":"assistant_text","message_id":"m","block_index":0,"text":"all done"}"#,
        )
        .await;
        // Two minutes pass with the runner silent: the final answer's clock is what the watchdog reads.
        rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
        check_all(&app).await;

        assert!(rt.final_text_at.lock().await.is_none(), "the claim is spent");
        let events = std::fs::read_to_string(&rt.events_path).unwrap();
        assert!(
            events.contains(r#""type":"watchdog_turn_end""#),
            "the end is on the record: {events}"
        );
        // The turn-end path ran: its autopilot decision is in the colony's log.
        let logs = rt.logs.lock().await.clone();
        assert!(
            logs.iter()
                .any(|l| l["message"].as_str().is_some_and(|m| m.contains("finishing the turn"))),
            "{logs:?}"
        );
        assert!(
            logs.iter().any(|l| l["message"]
                .as_str()
                .is_some_and(|m| m.contains("autopilot: not publishing yet"))),
            "the ordinary turn-end path ran: {logs:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Acceptance: a tool call in flight is not an ended turn, so it is left alone.
    #[tokio::test]
    async fn a_tool_call_in_flight_holds_the_turn_open() {
        let (app, root) = app_with_agentd("tool", r#"{"agent":{"running":true,"state":"working"}}"#).await;
        let rt = app.runtime("w1").await;
        rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
        rt.open_tool_calls.lock().await.insert("toolu_1".into());
        check_all(&app).await;
        assert!(
            rt.final_text_at.lock().await.is_some(),
            "not finished while a call is in flight"
        );
        assert!(
            !std::fs::read_to_string(&rt.events_path)
                .unwrap_or_default()
                .contains("watchdog_turn_end")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Acceptance: a request in flight through the gateway is progress, not a wedge.
    #[tokio::test]
    async fn a_gateway_request_in_flight_holds_the_turn_open() {
        let (app, root) = app_with_agentd("busy", r#"{"agent":{"running":true,"state":"working"}}"#).await;
        let rt = app.runtime("w1").await;
        rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
        app.gateway.colony_counter("w1").fetch_add(1, Ordering::SeqCst);
        check_all(&app).await;
        assert!(
            rt.final_text_at.lock().await.is_some(),
            "not finished while the gateway is busy"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Acceptance: the probe can take seconds, and a busy agent is not interrupted. If the runner
    /// starts a tool call while the probe is in flight, the end is not synthesised even though the
    /// probe answers that the runner is alive — the state is re-read after the probe.
    #[tokio::test]
    async fn a_tool_call_started_during_the_probe_holds_the_turn_open() {
        let (app, root) = stalled_app("race", SessionStatus::Running).await;
        let rt = app.runtime("w1").await;
        {
            let mut activity = rt.activity.lock().await;
            activity.last = Utc::now();
            activity.nudges = 0;
            activity.last_nudge = None;
            rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
        }
        // A health stub that opens a tool call on the real Runtime *before* it answers, standing in
        // for the runner beginning work during the probe's window.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = r#"{"agent":{"running":true,"state":"working"}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let stub_rt = rt.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let response = response.clone();
                let rt = stub_rt.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    rt.open_tool_calls.lock().await.insert("toolu_race".into());
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        let vm = app.session_dir("w1").join("vm");
        std::fs::create_dir_all(&vm).unwrap();
        std::fs::write(vm.join("token"), "test-token\n").unwrap();
        app.update_session("w1", |x| x.local_port = Some(port)).await;

        check_all(&app).await;

        assert!(
            rt.final_text_at.lock().await.is_some(),
            "the claim is kept: the runner started working during the probe"
        );
        assert!(
            !std::fs::read_to_string(&rt.events_path)
                .unwrap_or_default()
                .contains("watchdog_turn_end"),
            "no end is synthesised for a busy runner"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A colony whose agentd is gone is wedged, not finished: the recovery keeps the final answer so
    /// the next tick can retry, logs that it could not run once (not once per tick), and leaves the
    /// colony to the stall handling rather than restarting it.
    #[tokio::test]
    async fn an_unreachable_agentd_leaves_the_turn_to_the_stall_handling() {
        let (app, root) = stalled_app("wedged", SessionStatus::Running).await;
        let rt = app.runtime("w1").await;
        {
            let mut activity = rt.activity.lock().await;
            activity.last = Utc::now();
            activity.nudges = 0;
            activity.last_nudge = None;
            rt.final_text_at.lock().await.replace(Utc::now() - Duration::minutes(3));
        }
        // No token file and no local port: dial_agentd fails at once, as an unreachable agentd would.
        check_all(&app).await;
        check_all(&app).await;
        assert!(
            rt.final_text_at.lock().await.is_some(),
            "a probe that failed leaves the claim for the next tick to retry"
        );
        let logs = rt.logs.lock().await.clone();
        assert_eq!(
            logs.iter()
                .filter(|l| l["message"].as_str().is_some_and(|m| m.contains("agentd is not answering")))
                .count(),
            1,
            "logged once per final answer, not once per tick: {logs:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
