//! The recovery-path decision point (issue #586): what to do when a step fails — a provider error, a
//! tool failure, a watchdog stall or an `autopilot_held`. The harness always applied a fixed rule
//! here (nudge, or hold for a person); this is that rule made a point the optional Jev classifier may
//! answer instead, through the shared layer in `decide.rs`.
//!
//! The closed option set is [`Action`]; `ask_human` and `stop` are always offered, and nothing here
//! pushes, publishes or deletes. `off` asks nothing and leaves every call site exactly as it was;
//! `shadow` asks and records without applying the pick; `act` uses a confident pick, bounded by a
//! per-colony cap after which the point asks a human instead. Every ask writes a `decisions.jsonl`
//! row, and a grader task appends a second `kind: "outcome"` row once the window below has passed.

use crate::Shared;
use crate::config::{ModulesConfig, setting};
use crate::decide::{self, Decision, Miss, Mode};
use crate::modules::AgentModule;
use crate::sessions::Session;
use crate::util::{append_line, short_id};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::time::Duration;

/// How long after a recovery the grader waits before it asks whether the colony progressed. Long
/// enough for a retry or a narrowed task to show in the colony's activity, short enough to grade
/// within one sitting.
pub const RECOVERY_WINDOW: Duration = Duration::from_secs(600);

/// Why a step failed — the four classes a recovery is chosen for. Cheap, machine-readable labels
/// only: the failure's own free text never reaches Jev.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// `ToolFailure` is the name the option vocabulary uses; the lint's shared-name warning is not worth
// renaming it away from the class it names.
#[allow(clippy::enum_variant_names)]
pub enum Failure {
    /// A provider error — a gateway `model_error`, an exhausted quota, a failed fallback.
    ProviderError,
    /// A tool call the agent could not complete; in the vocabulary, but no call site raises one yet.
    #[allow(dead_code)]
    ToolFailure,
    /// The watchdog saw no progress for its stall window.
    Stall,
    /// Autopilot held a colony whose turn ended with an error.
    AutopilotHeld,
}

impl Failure {
    pub fn as_str(&self) -> &'static str {
        match self {
            Failure::ProviderError => "provider_error",
            Failure::ToolFailure => "tool_failure",
            Failure::Stall => "stall",
            Failure::AutopilotHeld => "autopilot_held",
        }
    }
}

/// One pick in the closed recovery option set. The names it writes to the ledger are the same
/// strings offered to Jev, so a pick and an option cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Try the same step again.
    RetrySame,
    /// Try the same tier on another configured provider — never offered today (see [`options`]).
    RetryOtherProvider,
    /// Split the remaining work into smaller subagent tasks.
    NarrowTask,
    /// Nudge the agent (the watchdog's own message).
    NudgeAgent,
    /// Stop recovering and ask a person.
    AskHuman,
    /// Stop pushing on this colony and leave it for a person.
    Stop,
}

impl Action {
    /// The option string, exactly as it is offered to Jev and written to the ledger.
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::RetrySame => "retry_same",
            Action::RetryOtherProvider => "retry_other_provider",
            Action::NarrowTask => "narrow_task",
            Action::NudgeAgent => "nudge_agent",
            Action::AskHuman => "ask_human",
            Action::Stop => "stop",
        }
    }

    /// Reads an option string back into an [`Action`], the way `decide.rs` matches a pick to its
    /// options: trimmed, case-insensitive, so a spelling alone cannot turn a real pick into a miss.
    fn parse(option: &str) -> Option<Action> {
        const ALL: [Action; 6] = [
            Action::RetrySame,
            Action::RetryOtherProvider,
            Action::NarrowTask,
            Action::NudgeAgent,
            Action::AskHuman,
            Action::Stop,
        ];
        ALL.into_iter()
            .find(|action| action.as_str().eq_ignore_ascii_case(option.trim()))
    }

    /// Whether an automatic recovery — one that keeps the colony working without a person — counts
    /// against the per-colony cap. `ask_human` and `stop` hand the colony over, so they never do.
    pub fn counts_toward_cap(&self) -> bool {
        !matches!(self, Action::AskHuman | Action::Stop)
    }

    /// The short message an automatic recovery sends the agent, in the harness's voice. `None` for
    /// the options that do not talk to the agent, so a call site falls back to its own rule.
    pub fn agent_message(&self) -> Option<&'static str> {
        match self {
            Action::RetrySame => Some(
                "Recovery check: the last step failed. Try it once more; if it fails again, stop and say plainly \
                 what is blocking you rather than repeating it.",
            ),
            Action::NarrowTask => Some(
                "Recovery check: that step is not landing. Break the remaining work into smaller subagent tasks \
                 and finish them one at a time.",
            ),
            Action::NudgeAgent => Some(
                "Recovery check: there has been no progress for a while. If a command or process is hanging, stop \
                 it and try another way; otherwise continue the task and report what you are doing.",
            ),
            // Never offered — no provider switch exists — so it gets no message: telling the agent to
            // split its task for a provider change would be wrong. Ask/stop simply hand over.
            Action::RetryOtherProvider | Action::AskHuman | Action::Stop => None,
        }
    }
}

/// The options offered for one failure class. `ask_human` and `stop` are always offered;
/// `retry_other_provider` only when `other_provider`; the rest by class — a stall is nudged or
/// narrowed, a failed turn or a provider error is retried or narrowed.
pub fn options(failure: Failure, other_provider: bool) -> Vec<&'static str> {
    let mut options = match failure {
        Failure::Stall => vec![Action::NudgeAgent.as_str(), Action::NarrowTask.as_str()],
        Failure::ProviderError | Failure::ToolFailure | Failure::AutopilotHeld => {
            vec![Action::RetrySame.as_str(), Action::NarrowTask.as_str()]
        }
    };
    if other_provider {
        options.push(Action::RetryOtherProvider.as_str());
    }
    options.push(Action::AskHuman.as_str());
    options.push(Action::Stop.as_str());
    options
}

/// The recovery point's settings: the mode, the confidence a pick must clear, and the per-colony cap.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub mode: Mode,
    pub act_confidence: f64,
    pub cap: u32,
}

/// Reads the recovery settings off the install's agent module, the way boot reads the routing
/// point's; an org can still switch every point off for its colonies with `jev: false`, which
/// [`handle`] checks before asking.
pub fn settings(modules: &ModulesConfig, agents: &[AgentModule]) -> Settings {
    let schema = crate::modules::schema_for("agent", &modules.agent.provider, agents);
    let choice = &modules.agent;
    let flag = |key: &str| setting(choice, &schema, key).and_then(Value::as_bool);
    Settings {
        mode: Mode::from_settings(
            flag("jev_recovery_act").unwrap_or(false),
            flag("jev_shadow_mode").unwrap_or(false),
        ),
        act_confidence: setting(choice, &schema, "jev_recovery_act_confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.8),
        cap: setting(choice, &schema, "jev_recovery_cap")
            .and_then(Value::as_u64)
            .unwrap_or(2)
            .min(u32::MAX as u64) as u32,
    }
}

/// Chooses the recovery for one failure, purely. Off and shadow stay on the caller's own `rule`; act
/// uses Jev's pick when it is confident enough and inside the colony's cap. Reaching the cap turns an
/// automatic pick into `ask_human`, so a colony is never kept alive by machine-chosen recoveries
/// without end. The `did` is the ledger's word for what happened: `rule`, `jev`, or `cap`.
pub fn choose(
    mode: Mode,
    result: &Result<Decision, Miss>,
    threshold: f64,
    used: u32,
    cap: u32,
    rule: &'static str,
) -> (Action, &'static str) {
    let fallback = Action::parse(rule).unwrap_or(Action::AskHuman);
    if mode != Mode::Act {
        return (fallback, "rule");
    }
    let Ok(decision) = result else {
        return (fallback, "rule");
    };
    let Some(action) = Action::parse(&decision.pick) else {
        return (fallback, "rule");
    };
    // A low confidence falls back to the rule; a NaN one fails this comparison the same way, as
    // routing's `>=` does.
    if decision.confidence >= threshold {
        // The cap only bounds the automatic recoveries; a pick that asks a person or stops is
        // honoured however many recoveries have already run.
        if action.counts_toward_cap() && used >= cap {
            return (Action::AskHuman, "cap");
        }
        return (action, "jev");
    }
    (fallback, "rule")
}

/// Grades one recovery against what happened: whether the colony made progress after the decision.
/// Pure, so the grading rule can be tested apart from the clock and the file.
pub fn grade(decided_at: DateTime<Utc>, last_progress: DateTime<Utc>, window: Duration) -> Value {
    json!({
        "progressed": last_progress > decided_at,
        "window_min": window.as_secs() / 60,
    })
}

/// Handles one failure at the recovery point: asks Jev when the mode is not off, records the row,
/// spends a cap slot for an automatic recovery, arms the grader, and returns the action the caller
/// must carry out alongside the ledger's word for it (`rule`, `jev` or `cap`). Off is a clean no-op:
/// it returns the caller's own rule and writes nothing, so a colony with the point off behaves
/// exactly as it did before.
pub async fn handle(
    app: &Shared,
    session: &Session,
    failure: Failure,
    rule: &'static str,
    other_provider: bool,
) -> (Action, &'static str) {
    let modules = app.modules.read().await.clone();
    let settings = settings(&modules, &app.agents);
    let fallback = Action::parse(rule).unwrap_or(Action::AskHuman);
    if !settings.mode.asks() {
        return (fallback, "rule");
    }
    let rt = app.runtimes.lock().await.get(&session.id).cloned();
    let (used, nudges, last) = match &rt {
        Some(rt) => {
            let activity = rt.activity.lock().await;
            (activity.recoveries, activity.nudges, activity.last)
        }
        None => (0, 0, Utc::now()),
    };
    let minutes = (Utc::now() - last).num_minutes().max(0) as u64;
    let options = options(failure, other_provider);
    // Metadata only, never free text: the failure class, how long since the last progress, and the
    // recovery bookkeeping, all of which the harness built itself.
    let context = json!({
        "failure": failure.as_str(),
        "minutes_since_progress": minutes,
        "nudges": nudges,
        "recoveries_used": used,
    });
    let org_allows = app.org_settings(&session.org).jev != Some(false);
    let result = decide::decide(&decide::RECOVERY_PATH, settings.mode, org_allows, &options, &context).await;
    let (action, did) = choose(settings.mode, &result, settings.act_confidence, used, settings.cap, rule);
    // The ask above can take up to 1.8 s, so the cap is re-checked and spent in one critical section:
    // two failures that both read room under it must not both pass. The loser falls back to a human.
    let (action, did) = match (did, &rt) {
        ("jev", Some(rt)) if action.counts_toward_cap() => {
            let mut activity = rt.activity.lock().await;
            if activity.recoveries >= settings.cap {
                (Action::AskHuman, "cap")
            } else {
                activity.recoveries += 1;
                (action, did)
            }
        }
        _ => (action, did),
    };
    let row = decide::row(&decide::RECOVERY_PATH, session, settings.mode, &options, &result, did);
    decide::record(app, &row).await;
    spawn_grader(app.clone(), row, RECOVERY_WINDOW);
    (action, did)
}

/// Queues one `user_message` command to a colony's agent under the given id prefix, which the origin
/// resolver reads back when the agent's side echoes it. The one place both call sites send a recovery
/// message, so the shape cannot drift between them.
pub fn send_user_message(rt: &crate::sessions::Runtime, prefix: &str, text: &str) {
    rt.send_command(json!({
        "type": "user_message",
        "id": format!("{prefix}-{}", short_id()),
        "text": text,
    }));
}

/// Sleeps the window after a decision, then appends the grade as a second `decisions.jsonl` row:
/// `kind: "outcome"`, same point and session, `outcome` filled. A grade is a measurement, not an
/// event, so nothing goes to the activity log; a colony gone by window-end is left ungraded.
fn spawn_grader(app: Shared, row: decide::Row, window: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(window).await;
        let Some(rt) = app.runtimes.lock().await.get(&row.session).cloned() else {
            return;
        };
        let last = rt.activity.lock().await.last;
        let mut graded = row;
        graded.kind = "outcome";
        graded.outcome = Some(grade(graded.ts, last, window));
        if let Ok(line) = serde_json::to_string(&graded)
            && let Err(e) = append_line(&app.decisions_file(), &line).await
        {
            app.storage_failed("append to the decisions ledger", &e).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::{Decision, Miss};
    use crate::jev::JEV_MODEL;

    const RULE: &str = "nudge_agent";

    fn decision(pick: &str, confidence: f64) -> Result<Decision, Miss> {
        Ok(Decision {
            pick: pick.to_string(),
            model: JEV_MODEL.into(),
            confidence,
            latency_ms: 5,
            estimated_cost_usd: 0.0001,
        })
    }

    /// `ask_human` and `stop` are always offered, `retry_other_provider` only with another provider,
    /// and no option is one the shared layer refuses to hand to Jev.
    #[test]
    fn the_offered_options_always_include_a_human_and_stop_and_never_a_forbidden_name() {
        // The same prefixes `decide::FORBIDDEN` refuses (checked there before any network call).
        const FORBIDDEN: [&str; 4] = ["publish.", "security.", "delete.", "destroy."];
        for failure in [
            Failure::ProviderError,
            Failure::ToolFailure,
            Failure::Stall,
            Failure::AutopilotHeld,
        ] {
            for other_provider in [false, true] {
                let options = options(failure, other_provider);
                assert!(options.contains(&"ask_human"), "{failure:?} offers a human");
                assert!(options.contains(&"stop"), "{failure:?} offers stop");
                assert_eq!(
                    options.contains(&"retry_other_provider"),
                    other_provider,
                    "{failure:?} offers another provider only when there is one"
                );
                for option in &options {
                    assert!(!FORBIDDEN.iter().any(|p| option.starts_with(p)), "{option} is refused");
                    assert!(Action::parse(option).is_some(), "{option} is a real action");
                }
            }
        }
        // A stall is nudged rather than retried; a failed turn is retried rather than nudged.
        assert!(options(Failure::Stall, false).contains(&"nudge_agent"));
        assert!(!options(Failure::Stall, false).contains(&"retry_same"));
        assert!(options(Failure::AutopilotHeld, false).contains(&"retry_same"));
        assert!(!options(Failure::AutopilotHeld, false).contains(&"nudge_agent"));
    }

    /// Off asks nothing and shadow never acts: both return the caller's own rule.
    #[test]
    fn off_and_shadow_stay_on_the_rule_and_never_apply_a_pick() {
        for mode in [Mode::Off, Mode::Shadow] {
            let (action, did) = choose(mode, &decision("narrow_task", 0.99), 0.8, 0, 2, RULE);
            assert_eq!(action, Action::NudgeAgent, "the rule's action, whatever Jev said");
            assert_eq!(did, "rule");
        }
    }

    /// In act mode a confident, in-options pick is used, and a low confidence or a miss is not.
    #[test]
    fn act_uses_a_confident_pick_and_falls_back_below_the_threshold() {
        let (action, did) = choose(Mode::Act, &decision("narrow_task", 0.9), 0.8, 0, 2, RULE);
        assert_eq!(action, Action::NarrowTask);
        assert_eq!(did, "jev");

        let (action, did) = choose(Mode::Act, &decision("narrow_task", 0.5), 0.8, 0, 2, RULE);
        assert_eq!(action, Action::NudgeAgent, "below the threshold, the rule stands");
        assert_eq!(did, "rule");

        let (action, did) = choose(Mode::Act, &Err(Miss::NoKey), 0.8, 0, 2, RULE);
        assert_eq!(action, Action::NudgeAgent);
        assert_eq!(did, "rule");
    }

    /// The cap is enforced: an automatic pick at the cap becomes `ask_human`, below it is used, and
    /// `did` names why. A pick that hands the colony over is honoured whatever the count.
    #[test]
    fn the_cap_turns_a_further_automatic_recovery_into_ask_human() {
        let pick = decision("narrow_task", 0.9);
        assert_eq!(choose(Mode::Act, &pick, 0.8, 1, 2, RULE), (Action::NarrowTask, "jev"));
        assert_eq!(choose(Mode::Act, &pick, 0.8, 2, 2, RULE), (Action::AskHuman, "cap"));

        // ask_human and stop do not count toward the cap, so they are honoured even when it is spent.
        assert_eq!(
            choose(Mode::Act, &decision("ask_human", 0.9), 0.8, 5, 2, RULE),
            (Action::AskHuman, "jev")
        );
        assert_eq!(
            choose(Mode::Act, &decision("stop", 0.9), 0.8, 5, 2, RULE),
            (Action::Stop, "jev")
        );

        assert!(Action::RetrySame.counts_toward_cap() && Action::NarrowTask.counts_toward_cap());
        assert!(!Action::AskHuman.counts_toward_cap() && !Action::Stop.counts_toward_cap());
    }

    /// The grade is progress strictly after the decision, with the window it covers.
    #[test]
    fn grading_reads_progress_after_the_decision() {
        let decided = DateTime::from_timestamp(1_789_000_000, 0).unwrap();
        let window = Duration::from_secs(600);
        assert_eq!(
            grade(decided, decided, window),
            json!({"progressed": false, "window_min": 10})
        );
        assert_eq!(
            grade(decided, decided + chrono::Duration::minutes(1), window),
            json!({"progressed": true, "window_min": 10})
        );
        assert_eq!(
            grade(decided, decided - chrono::Duration::minutes(1), window),
            json!({"progressed": false, "window_min": 10})
        );
    }

    /// The settings come off the agent module: shadow is the shared switch, act its own, with the
    /// confidence and cap defaults when nothing is set.
    #[test]
    fn the_settings_read_the_agent_module_flags() {
        let mut modules = ModulesConfig::default();
        assert_eq!(settings(&modules, &[]).mode, Mode::Off);

        modules.agent.settings.insert("jev_shadow_mode".into(), json!(true));
        let read = settings(&modules, &[]);
        assert_eq!(read.mode, Mode::Shadow);
        assert_eq!(read.act_confidence, 0.8);
        assert_eq!(read.cap, 2);

        modules.agent.settings.insert("jev_recovery_act".into(), json!(true));
        assert_eq!(settings(&modules, &[]).mode, Mode::Act, "act wins over shadow");
    }
}
