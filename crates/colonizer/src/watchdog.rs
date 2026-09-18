//! Watchdog module: notices colonies that stopped making progress, nudges them, and flags them for
//! the user when nudging doesn't help. The decision is a pure function so it can be tested with a
//! fixed clock; the loop around it runs once a minute.

use crate::{Shared, orgs::effective_watchdog, sessions::SessionStatus, util::short_id};
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
}

impl Activity {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            last: now,
            nudges: 0,
            last_nudge: None,
            question_since: None,
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
const WATCHDOG_REASONS: [&str; 3] = ["stalled", "waiting_for_answer", "nudges_exhausted"];

pub fn decide(
    settings: &WatchdogSettings,
    now: DateTime<Utc>,
    state: Observed,
    activity: &Activity,
    attention: Option<&str>,
) -> Decision {
    if !settings.enabled {
        let ours = attention.is_some_and(|reason| WATCHDOG_REASONS.contains(&reason));
        return if ours {
            Decision::Clear
        } else {
            Decision::Nothing
        };
    }
    match state {
        Observed::WaitingForAnswer => {
            let waited = activity
                .question_since
                .map(|since| now - since)
                .unwrap_or_else(Duration::zero);
            if waited >= Duration::minutes(settings.waiting_minutes as i64)
                && attention != Some("waiting_for_answer")
            {
                Decision::Flag("waiting_for_answer")
            } else {
                Decision::Nothing
            }
        }
        Observed::Working => {
            let reference = activity
                .last_nudge
                .map_or(activity.last, |nudge| nudge.max(activity.last));
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
    let sessions = app.sessions.read().await.clone();
    let modules = app.modules.read().await.clone();
    let now = Utc::now();
    for s in sessions
        .into_iter()
        .filter(|s| s.status.is_live() && s.status != SessionStatus::Starting)
    {
        let Some(rt) = app.runtimes.lock().await.get(&s.id).cloned() else {
            continue;
        };
        let settings = effective_watchdog(&modules, &app.org_settings(&s.org));
        let state = match s.status {
            SessionStatus::Running => Observed::Working,
            SessionStatus::WaitingForAnswer => Observed::WaitingForAnswer,
            _ => Observed::Other,
        };
        let attention = s
            .attention
            .as_ref()
            .and_then(|a| a["reason"].as_str())
            .map(String::from);
        // A request waiting on a slow model through the gateway is progress, not a stall.
        if app.gateway.colony_busy(&s.id) {
            {
                let mut current = rt.activity.lock().await;
                current.last = now;
                current.nudges = 0;
                current.last_nudge = None;
            }
            if attention
                .as_deref()
                .is_some_and(|reason| reason != "waiting_for_answer")
            {
                app.update_session(&s.id, |x| x.attention = None).await;
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
                app.session_log(
                    &s.id,
                    "info",
                    format!(
                        "watchdog: no progress for {} min, nudged the agent ({nudges}/{})",
                        settings.stall_minutes, settings.max_nudges
                    ),
                )
                .await;
                app.update_session(&s.id, |x| {
                    x.attention = Some(
                        json!({"reason": "stalled", "since": activity.last, "nudges": nudges}),
                    );
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
                app.session_log(&s.id, "error", message).await;
                let since = if reason == "waiting_for_answer" {
                    activity.question_since.unwrap_or(now)
                } else {
                    activity.last
                };
                app.update_session(&s.id, |x| {
                    x.attention =
                        Some(json!({"reason": reason, "since": since, "nudges": activity.nudges}));
                })
                .await;
            }
            Decision::Clear => {
                app.update_session(&s.id, |x| x.attention = None).await;
            }
        }
    }
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
        assert_eq!(
            decide(&SETTINGS, at(15), Observed::Working, &activity, None),
            Decision::Nudge
        );

        activity.nudges = 1;
        activity.last_nudge = Some(at(15));
        assert_eq!(
            decide(
                &SETTINGS,
                at(20),
                Observed::Working,
                &activity,
                Some("stalled")
            ),
            Decision::Nothing
        );
        assert_eq!(
            decide(
                &SETTINGS,
                at(30),
                Observed::Working,
                &activity,
                Some("stalled")
            ),
            Decision::Nudge
        );

        activity.nudges = 2;
        activity.last_nudge = Some(at(30));
        assert_eq!(
            decide(
                &SETTINGS,
                at(44),
                Observed::Working,
                &activity,
                Some("stalled")
            ),
            Decision::Nothing
        );
        assert_eq!(
            decide(
                &SETTINGS,
                at(45),
                Observed::Working,
                &activity,
                Some("stalled")
            ),
            Decision::Flag("nudges_exhausted")
        );
        assert_eq!(
            decide(
                &SETTINGS,
                at(90),
                Observed::Working,
                &activity,
                Some("nudges_exhausted")
            ),
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
        };
        assert_eq!(
            decide(&SETTINGS, at(34), Observed::Working, &activity, None),
            Decision::Nothing
        );
        assert_eq!(
            decide(&SETTINGS, at(35), Observed::Working, &activity, None),
            Decision::Nudge
        );
    }

    #[test]
    fn unanswered_questions_are_flagged_not_nudged() {
        let activity = Activity {
            last: at(0),
            nudges: 0,
            last_nudge: None,
            question_since: Some(at(0)),
        };
        assert_eq!(
            decide(
                &SETTINGS,
                at(29),
                Observed::WaitingForAnswer,
                &activity,
                None
            ),
            Decision::Nothing
        );
        assert_eq!(
            decide(
                &SETTINGS,
                at(30),
                Observed::WaitingForAnswer,
                &activity,
                None
            ),
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
            decide(
                &SETTINGS,
                at(61),
                Observed::Other,
                &activity,
                Some("waiting_for_answer")
            ),
            Decision::Clear
        );
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
            decide(
                &settings,
                at(500),
                Observed::Working,
                &activity,
                Some("stalled")
            ),
            Decision::Clear
        );
        assert_eq!(
            decide(
                &settings,
                at(500),
                Observed::Other,
                &activity,
                Some("autopilot_held")
            ),
            Decision::Nothing
        );
        assert_eq!(
            decide(
                &SETTINGS,
                at(500),
                Observed::Other,
                &activity,
                Some("autopilot_held")
            ),
            Decision::Nothing
        );
    }
}
