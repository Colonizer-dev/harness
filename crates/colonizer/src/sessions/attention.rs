//! The attention flag a colony raises when it needs the operator, and its startup cleanup.

use super::*;

/// The one-line history note for an attention flag a terminal transition just removed: the reason
/// it was set, so the colony's log still says what the flag meant after the flag itself is gone.
/// `None` when there was no flag, so callers only log when something was actually cleared.
pub(crate) fn cleared_attention_message(attention: &Option<Value>) -> Option<String> {
    let attention = attention.as_ref()?;
    let reason = attention.get("reason").and_then(Value::as_str).unwrap_or("unknown");
    Some(format!(
        "clearing the attention flag ({reason}): the colony is not running, so nothing is waiting on it any more"
    ))
}

/// Startup migration, run in `serve` next to the org backfill and before `recover`: colonies
/// persisted as finished while still carrying an attention flag predate the clearing every
/// terminal transition now does. A finished colony that still carries one looks like it needs
/// attention it no longer does, so drop the flag from every terminal colony that has one — except a
/// quota-parked colony, whose flag is its resume ticket: stripping it would strand the colony,
/// parked with no reason for the queue to ever requeue. Returns how many flags were cleared.
pub(crate) fn clear_stale_attention(sessions: &mut [Session]) -> usize {
    let mut cleared = 0;
    for s in sessions.iter_mut() {
        if s.status.is_terminal() && s.attention.is_some() {
            let quota_parked = s
                .attention
                .as_ref()
                .is_some_and(|a| a["reason"].as_str() == Some(crate::provider_quota::QUOTA_EXHAUSTED_REASON));
            if quota_parked {
                continue;
            }
            s.attention = None;
            cleared += 1;
        }
    }
    cleared
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::sessions::tests::*;

    #[test]
    fn startup_migration_clears_stale_attention_only_on_finished_colonies() {
        fn flagged(status: SessionStatus) -> Session {
            let mut s = colony("acme", status);
            s.attention = Some(json!({"reason": "stalled", "since": Utc::now(), "nudges": 2}));
            s
        }
        let mut sessions = vec![
            flagged(SessionStatus::Stopped),
            flagged(SessionStatus::Failed),
            flagged(SessionStatus::PrOpened),
            flagged(SessionStatus::Running),
            colony("acme", SessionStatus::Stopped),
        ];
        assert_eq!(clear_stale_attention(&mut sessions), 3);
        for s in &sessions[..3] {
            assert!(s.attention.is_none(), "{:?} must not keep a stale attention flag", s.status);
        }
        assert!(
            sessions[3].attention.is_some(),
            "a live colony keeps the flag the watchdog is still managing"
        );
        assert!(
            sessions[4].attention.is_none(),
            "a finished colony without a flag is untouched"
        );
        assert_eq!(clear_stale_attention(&mut sessions), 0, "the migration is idempotent");
    }

    #[test]
    fn startup_migration_keeps_a_quota_parked_colony_resumable() {
        let mut parked = stopped_colony_with_worktree("acme", "parked".into());
        parked.status = SessionStatus::Stopped;
        parked.error = Some("provider quota exhausted (resets 7am (UTC))".into());
        parked.attention =
            Some(json!({"reason": crate::provider_quota::QUOTA_EXHAUSTED_REASON, "since": Utc::now(), "nudges": 0}));
        let mut sessions = vec![parked];
        assert_eq!(
            clear_stale_attention(&mut sessions),
            0,
            "the park reason is the resume ticket, not stale"
        );
        assert_eq!(
            sessions[0].attention.as_ref().and_then(|a| a["reason"].as_str()),
            Some(crate::provider_quota::QUOTA_EXHAUSTED_REASON),
            "a restart must not strand the parked colony"
        );
    }
}
