//! Parking colonies that sit idle (issue #1140).
//!
//! A colony that is `idle`, whether quietly, held by autopilot or flagged by the watchdog, keeps a
//! microVM and a parallel slot while it does nothing, and the only older bound was the 90 minute
//! hold timeout. Once it has had no open question and no publish in flight for `idle_park_minutes`
//! (a watchdog setting, default 15), the queue parks it through the same path every other park
//! takes: the microVM goes, the worktree and branch stay, and Resume brings it back. A colony with
//! an open question keeps its old behaviour (the grace and the question cap), so it is never parked
//! here.

use crate::{
    Shared, lifecycle,
    sessions::{Session, SessionStatus},
};
use chrono::{DateTime, Utc};
use serde_json::Value;

/// The attention reason (and park reason) an idle-parked colony carries. It is not a call for a
/// person: the colony only waits to be resumed, so the cockpit does not list it under Needs you.
pub(crate) const IDLE_PARK_REASON: &str = "idle_timeout";

/// The one message an idle colony gets before it parks when its last turn ended without writing or
/// updating its PR description: asking again costs one turn, and often saves the colony.
pub(crate) const PR_REWRITE_MESSAGE: &str = "Your last turn ended without writing or updating your PR description, so \
     autopilot cannot publish. Rewrite /harness/out/pr.md now so it describes this change. If an earlier draft was \
     redacted because it contained a secret, leave that value out entirely.";

/// The nudge, naming what redaction found in the draft on disk (issue #1175): the line and the kind
/// of each match, never the value. Without a finding it is [`PR_REWRITE_MESSAGE`] as it was.
pub(crate) fn pr_rewrite_message(findings: &[(usize, String)]) -> String {
    if findings.is_empty() {
        return PR_REWRITE_MESSAGE.to_string();
    }
    let named = findings
        .iter()
        .take(10)
        .map(|(line, kind)| format!("line {line} matched {kind}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!("{PR_REWRITE_MESSAGE} In the draft on disk: {named}. Rewrite those sentences without the value.")
}

/// The reason `events::autopilot_step` gives for a turn that did not touch `pr.md`.
pub(crate) const PR_NOT_WRITTEN: &str = "the agent didn't write or update its PR description this turn";

/// Whether the colony's own record says it has been idle long enough to park at `now`, before the
/// two facts only the runtime knows — an open question, a verification in flight — are consulted.
/// Pure, so the policy is testable apart from the tick that acts on it. Only `Idle` qualifies: a
/// `WaitingForAnswer` colony is a question, a `Publishing` one is a publish in flight, and a
/// suspended or pre-warming colony is already managed by the question path.
pub(crate) fn idle_park_due(s: &Session, now: DateTime<Utc>, timeout: chrono::Duration) -> bool {
    if s.status != SessionStatus::Idle || s.cleaned_up {
        return false;
    }
    if s.suspended.is_some() || s.pending_answer.is_some() || s.prewarm.is_some() {
        return false;
    }
    if s.attention
        .as_ref()
        .and_then(|a| a.get("reason"))
        .and_then(Value::as_str)
        .is_some_and(|reason| reason == "waiting_for_answer")
    {
        return false;
    }
    // Any real change to the record (a status change, an attention flag, a nudge) restarts the
    // clock, so a colony the watchdog is still working on is not parked under its feet.
    now - s.updated_at >= timeout
}

/// Whether autopilot's PR-description wait should get its one automatic message: the turn ended
/// without a PR description, the colony is on autopilot and has not been asked before.
pub(crate) fn wants_pr_rewrite(reason: &str, s: &Session) -> bool {
    reason == PR_NOT_WRITTEN && s.autopilot && !s.pr_rewrite_nudged
}

/// Parks every idle colony past `timeout` (issue #1140), on the queue tick ahead of admission so the
/// slots it frees are visible to the loop that follows. Each candidate is re-checked against the
/// runtime — no open question, no verification running — and against its record under the park's own
/// claim, which re-checks that it is still live.
pub(crate) async fn park_idle(app: &Shared, timeout: chrono::Duration) {
    let now = Utc::now();
    let ids: Vec<String> = {
        app.sessions
            .read()
            .await
            .iter()
            .filter(|s| idle_park_due(s, now, timeout))
            .map(|s| s.id.clone())
            .collect()
    };
    let minutes = timeout.num_minutes();
    for id in ids {
        let rt = app.runtime(&id).await;
        if rt.open_question().await.is_some() || rt.verify_lock.try_lock().is_err() {
            continue;
        }
        // Read again after the awaits: an answer or a nudge in between restarts the clock.
        let Some(s) = app.session(&id).await.filter(|s| idle_park_due(s, Utc::now(), timeout)) else {
            continue;
        };
        lifecycle::park_colony(
            app,
            &s,
            IDLE_PARK_REASON,
            None,
            format!("idle for {minutes} min with nothing to do; parked to release its slot — the worktree is kept, so press Resume to continue"),
            format!("idle for {minutes} min with no question and no publish in flight; parked to release its slot — worktree kept, resume to continue"),
        )
        .await;
    }
}

#[cfg(test)]
mod tests;
