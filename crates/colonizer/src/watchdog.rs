//! Watchdog module: notices colonies that stopped making progress, nudges them, and flags them for
//! the user when nudging doesn't help. The decision is a pure function so it can be tested with a
//! fixed clock; the loop around it runs once a minute.

use crate::{Shared, orgs::effective_watchdog, protocol::Origin, sessions::SessionStatus, util::short_id};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

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
                let nudges = activity.nudges + 1;
                {
                    let mut current = rt.activity.lock().await;
                    current.nudges = nudges;
                    current.last_nudge = Some(now);
                }
                let command = json!({
                    "type": "user_message",
                    "id": format!("watchdog-{}", short_id()),
                    "text": nudge_text(settings.stall_minutes),
                });
                rt.send_command(command);
                app.session_log_as(
                    Origin::Watchdog,
                    &s.id,
                    "info",
                    format!(
                        "watchdog: no progress for {} min, nudged the agent ({nudges}/{})",
                        settings.stall_minutes, settings.max_nudges
                    ),
                )
                .await;
                app.update_session(&s.id, |x| {
                    x.attention = Some(json!({"reason": "stalled", "since": activity.last, "nudges": nudges}));
                })
                .await;
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
}
