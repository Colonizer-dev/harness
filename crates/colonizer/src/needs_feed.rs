//! The "Needs you" list as the app badge counts it (issue #1140), mirroring `needsYouFeed` in
//! web/src/notifications.ts — keep the two in step. [`crate::push::needs_you`] says whether one
//! colony needs a person; this removes the noise around the list: failed or stopped colonies a newer
//! colony for the same issue has overtaken, cascade failures, all but the newest colony per issue,
//! and abandoned questions older than 72 hours, which fold into one row.

use crate::sessions::{Session, SessionStatus};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// How old an abandoned question must be before the list folds it into the "old questions" row.
pub(crate) const OLD_QUESTION: chrono::Duration = chrono::Duration::hours(72);

/// Whether a failure only exists because the colony it was stacked on stopped or failed. The queue
/// no longer produces these (a dependent is blocked or re-based), but records written before that do.
pub(crate) fn is_cascade_failure(s: &Session) -> bool {
    if s.status != SessionStatus::Failed {
        return false;
    }
    let error = s.error.as_deref().unwrap_or_default().to_lowercase();
    error.contains("no branch to build on") || error.contains("cannot be stacked on") || error.contains("stopped or parked, so")
}

/// Whether a failed or stopped colony has a newer colony for the same issue that is queued, blocked,
/// running, open or merged.
fn superseded(s: &Session, all: &[Session]) -> bool {
    if !matches!(s.status, SessionStatus::Failed | SessionStatus::Stopped) {
        return false;
    }
    let Some(issue) = s.issue else { return false };
    all.iter().any(|o| {
        o.id != s.id
            && o.repo == s.repo
            && o.issue == Some(issue)
            && o.created_at > s.created_at
            && matches!(
                o.status,
                SessionStatus::Queued
                    | SessionStatus::Blocked
                    | SessionStatus::Starting
                    | SessionStatus::Running
                    | SessionStatus::WaitingForAnswer
                    | SessionStatus::Idle
                    | SessionStatus::Publishing
                    | SessionStatus::PrOpened
                    | SessionStatus::Merged
            )
    })
}

/// How many rows the list has at `now`: one per colony still asking for a person, plus one for all
/// the folded old questions together.
pub(crate) fn rows(sessions: &[Session], now: DateTime<Utc>) -> usize {
    let candidates: Vec<&Session> = sessions
        .iter()
        .filter(|s| crate::push::needs_you(s) && !is_cascade_failure(s) && !superseded(s, sessions))
        .collect();
    let mut newest: HashMap<(&str, u64), &Session> = HashMap::new();
    for s in &candidates {
        if let Some(issue) = s.issue {
            let held = newest.entry((s.repo.as_str(), issue)).or_insert(s);
            if s.created_at > held.created_at {
                *held = s;
            }
        }
    }
    let mut count = 0;
    let mut old = false;
    for s in candidates {
        if let Some(issue) = s.issue
            && newest.get(&(s.repo.as_str(), issue)).is_some_and(|n| n.id != s.id)
        {
            continue;
        }
        let abandoned = s.status == SessionStatus::Failed && s.error.as_deref() == Some(crate::queue::ABANDONED_QUESTION_REASON);
        if abandoned && now - s.updated_at > OLD_QUESTION {
            old = true;
        } else {
            count += 1;
        }
    }
    count + usize::from(old)
}

#[cfg(test)]
mod tests;
