//! One health state per fleet member (issue #764): `ok`, `degraded` or `stopped`, with the reason
//! and one thing to do about it. Members fail silently — a machine asleep, a revoked token, a full
//! disk — and the owner's member list should say which, and what next, without a log dive.
//!
//! A member the owner has not polled yet reads `unknown`, never `ok`: nobody checked it.
//!
//! [`evaluate`] is the whole rule and is pure: it takes the [`Signals`] observed for one member
//! and answers a [`Health`]. Every signal that fires becomes a finding; the worst severity wins, and
//! a tie between two findings of the same severity breaks by [`Reason::priority`], a fixed order.
//!
//! Which signals are wired today (see `fleet_members::member_signals`):
//! - `heartbeat_age`, `reachable`, `disk_free_bytes` / `disk_total_bytes`: from the owner's own
//!   polls of the member's `/api/status` (`fleet::list_hosts`, the `GET /api/hosts` fan-out).
//! - `token_valid`: whether the member's fleet token is still in the token registry.
//! - `has_url`: whether the member published a URL the owner can poll at all.
//!
//! Defined but not yet wired — nothing in the harness measures them yet, so they stay `None` and
//! never fire: `sync_backlog_age`, `last_sync_error`, `runner_alive`.

use chrono::Duration;
use serde::Serialize;

/// Heartbeat age past which a member is degraded, then stopped.
const HEARTBEAT_DEGRADED: Duration = Duration::minutes(5);
const HEARTBEAT_STOPPED: Duration = Duration::minutes(30);
/// Sync backlog age past which a member is degraded, then stopped.
const BACKLOG_DEGRADED: Duration = Duration::minutes(10);
const BACKLOG_STOPPED: Duration = Duration::minutes(60);
/// Disk use, in percent, past which a member is degraded, then stopped.
const DISK_DEGRADED_PERCENT: u64 = 90;
const DISK_STOPPED_PERCENT: u64 = 95;
/// Free bytes past which a member is degraded, then stopped — the fallback when a peer reports its
/// free space but not its total.
const GIB: u64 = 1024 * 1024 * 1024;
const DISK_DEGRADED_FREE: u64 = 5 * GIB;
const DISK_STOPPED_FREE: u64 = GIB;

/// The class of the member's last failed sync, when it has one. Nothing records sync errors yet,
/// so only the tests construct these; the rule is ready for when something does.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncError {
    /// 401: the credential is gone.
    Unauthorized,
    /// 403: the credential no longer reaches what it syncs.
    Forbidden,
    /// 429: the other side is throttling it.
    RateLimited,
}

/// Everything observed about one member. `None` means "not measured", never "fine" or "bad": a
/// signal that is not measured cannot fire.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signals {
    /// How long before the owner's latest poll the member last answered one (wired).
    pub heartbeat_age: Option<Duration>,
    /// Whether the owner's latest poll reached the member (wired).
    pub reachable: Option<bool>,
    /// The member's disk, as its own status reports it (wired).
    pub disk_free_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    /// Whether the member's fleet token is still in the registry (wired).
    pub token_valid: Option<bool>,
    /// Whether the member published a URL the owner can poll (wired). `false` leaves it unwatched.
    pub has_url: bool,
    /// How long the oldest unsynced item has waited (defined only).
    pub sync_backlog_age: Option<Duration>,
    /// The class of the last failed sync (defined only).
    pub last_sync_error: Option<SyncError>,
    /// Whether the member's colony runner is alive (defined only).
    pub runner_alive: Option<bool>,
}

/// The state, worst last: the derived order is what "the worst state wins" compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ok,
    /// Not checked yet: no poll has reached a verdict, so nothing can be claimed either way.
    Unknown,
    Degraded,
    Stopped,
}

/// Why a member is not ok. Declaration order is the tie priority: first wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    TokenRevoked,
    SyncRejected(SyncError),
    RunnerDown,
    DiskFull { percent: Option<u64>, free_bytes: u64 },
    NoHeartbeat { minutes: i64 },
    Unreachable,
    SyncBacklog { minutes: i64 },
    SyncRateLimited,
    Unwatched,
    NotChecked,
}

impl Reason {
    /// The tie order among findings of the same severity: lower comes first.
    pub fn priority(&self) -> u8 {
        match self {
            Reason::TokenRevoked => 0,
            Reason::SyncRejected(_) => 1,
            Reason::RunnerDown => 2,
            Reason::DiskFull { .. } => 3,
            Reason::NoHeartbeat { .. } => 4,
            Reason::Unreachable => 5,
            Reason::SyncBacklog { .. } => 6,
            Reason::SyncRateLimited => 7,
            Reason::Unwatched => 8,
            Reason::NotChecked => 9,
        }
    }

    /// A stable machine key, for the cockpit and scripts.
    pub fn code(&self) -> &'static str {
        match self {
            Reason::TokenRevoked => "token_revoked",
            Reason::SyncRejected(_) => "sync_rejected",
            Reason::RunnerDown => "runner_down",
            Reason::DiskFull { .. } => "disk_full",
            Reason::NoHeartbeat { .. } => "no_heartbeat",
            Reason::Unreachable => "unreachable",
            Reason::SyncBacklog { .. } => "sync_backlog",
            Reason::SyncRateLimited => "sync_rate_limited",
            Reason::Unwatched => "unwatched",
            Reason::NotChecked => "not_checked",
        }
    }

    /// What happened, in a few words.
    pub fn text(&self) -> String {
        match self {
            Reason::TokenRevoked => "Token revoked".into(),
            Reason::SyncRejected(SyncError::Forbidden) => "Sync refused (403)".into(),
            Reason::SyncRejected(_) => "Sync unauthorized (401)".into(),
            Reason::RunnerDown => "Colony runner is not running".into(),
            Reason::DiskFull {
                percent: Some(percent), ..
            } => format!("Disk {percent}% full"),
            Reason::DiskFull { free_bytes, .. } => format!("Disk has {} free", human_bytes(*free_bytes)),
            Reason::NoHeartbeat { minutes } => format!("No heartbeat for {}", human_minutes(*minutes)),
            Reason::Unreachable => "Not answering".into(),
            Reason::SyncBacklog { minutes } => format!("Sync stuck for {}", human_minutes(*minutes)),
            Reason::SyncRateLimited => "Sync rate-limited (429)".into(),
            Reason::Unwatched => "Not watched".into(),
            Reason::NotChecked => "Not checked yet".into(),
        }
    }

    /// The one thing to do about it.
    pub fn hint(&self) -> &'static str {
        match self {
            Reason::TokenRevoked | Reason::SyncRejected(SyncError::Unauthorized) => "re-pair this machine",
            Reason::SyncRejected(_) => "re-pair this machine so its token gets the access it needs",
            Reason::RunnerDown => "restart colonizer on this machine",
            Reason::DiskFull { .. } => "clean target/ dirs",
            Reason::NoHeartbeat { .. } => "the machine may be asleep",
            Reason::Unreachable => "check that it is awake and on the network",
            Reason::SyncBacklog { .. } => "check its network, then restart colonizer there",
            Reason::SyncRateLimited => "it backs off by itself; wait a few minutes",
            Reason::Unwatched => "re-join with this machine's URL so the owner can poll it",
            Reason::NotChecked => "open the cockpit or wait for the next poll",
        }
    }
}

/// One member's verdict. `reason` is `None` exactly when `state` is `Ok`.
#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    pub state: State,
    pub reason: Option<Reason>,
}

impl Health {
    /// The API shape: `{state, code, reason, hint}`, the last three null when ok.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state,
            "code": self.reason.as_ref().map(Reason::code),
            "reason": self.reason.as_ref().map(Reason::text),
            "hint": self.reason.as_ref().map(Reason::hint),
        })
    }
}

/// The rule: every signal that fires is a finding; the worst state wins, ties by priority.
pub fn evaluate(signals: &Signals) -> Health {
    let mut findings: Vec<(State, Reason)> = Vec::new();

    if signals.token_valid == Some(false) {
        findings.push((State::Stopped, Reason::TokenRevoked));
    }
    match signals.last_sync_error {
        Some(error @ (SyncError::Unauthorized | SyncError::Forbidden)) => {
            findings.push((State::Stopped, Reason::SyncRejected(error)));
        }
        Some(SyncError::RateLimited) => findings.push((State::Degraded, Reason::SyncRateLimited)),
        None => {}
    }
    if signals.runner_alive == Some(false) {
        findings.push((State::Stopped, Reason::RunnerDown));
    }
    if let Some(free) = signals.disk_free_bytes {
        match signals.disk_total_bytes.filter(|total| *total > 0) {
            Some(total) => {
                let percent = total.saturating_sub(free).saturating_mul(100) / total;
                let state = if percent >= DISK_STOPPED_PERCENT {
                    Some(State::Stopped)
                } else if percent >= DISK_DEGRADED_PERCENT {
                    Some(State::Degraded)
                } else {
                    None
                };
                if let Some(state) = state {
                    findings.push((
                        state,
                        Reason::DiskFull {
                            percent: Some(percent),
                            free_bytes: free,
                        },
                    ));
                }
            }
            None => {
                let state = if free < DISK_STOPPED_FREE {
                    Some(State::Stopped)
                } else if free < DISK_DEGRADED_FREE {
                    Some(State::Degraded)
                } else {
                    None
                };
                if let Some(state) = state {
                    findings.push((
                        state,
                        Reason::DiskFull {
                            percent: None,
                            free_bytes: free,
                        },
                    ));
                }
            }
        }
    }
    if let Some(age) = signals.heartbeat_age {
        let minutes = age.num_minutes();
        if age >= HEARTBEAT_STOPPED {
            findings.push((State::Stopped, Reason::NoHeartbeat { minutes }));
        } else if age >= HEARTBEAT_DEGRADED {
            findings.push((State::Degraded, Reason::NoHeartbeat { minutes }));
        }
    }
    if signals.reachable == Some(false) {
        findings.push((State::Degraded, Reason::Unreachable));
    }
    if let Some(age) = signals.sync_backlog_age {
        let minutes = age.num_minutes();
        if age >= BACKLOG_STOPPED {
            findings.push((State::Stopped, Reason::SyncBacklog { minutes }));
        } else if age >= BACKLOG_DEGRADED {
            findings.push((State::Degraded, Reason::SyncBacklog { minutes }));
        }
    }
    if !signals.has_url {
        findings.push((State::Degraded, Reason::Unwatched));
    } else if signals.reachable.is_none() {
        // Polled never: whatever else fires still wins, but on its own this is not `ok`.
        findings.push((State::Unknown, Reason::NotChecked));
    }

    // Worst state first, then the lowest priority number.
    match findings
        .into_iter()
        .min_by_key(|(state, reason)| (std::cmp::Reverse(*state), reason.priority()))
    {
        Some((state, reason)) => Health {
            state,
            reason: Some(reason),
        },
        None => Health {
            state: State::Ok,
            reason: None,
        },
    }
}

/// "12 min", "3 h", "2 d" — the scale a person reads a gap at.
fn human_minutes(minutes: i64) -> String {
    if minutes < 120 {
        format!("{minutes} min")
    } else if minutes < 48 * 60 {
        format!("{} h", minutes / 60)
    } else {
        format!("{} d", minutes / (24 * 60))
    }
}

/// "800 MB", "3.2 GB".
fn human_bytes(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MB", bytes / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A member everything is fine with: watched, answering, recent, roomy, token live.
    fn healthy() -> Signals {
        Signals {
            heartbeat_age: Some(Duration::seconds(20)),
            reachable: Some(true),
            disk_free_bytes: Some(400 * GIB),
            disk_total_bytes: Some(500 * GIB),
            token_valid: Some(true),
            has_url: true,
            ..Signals::default()
        }
    }

    #[test]
    fn signal_combinations_map_to_one_state_and_reason() {
        type Case = (
            &'static str,
            fn(&mut Signals),
            State,
            Option<&'static str>,
            Option<&'static str>,
        );
        let cases: &[Case] = &[
            ("all fine", |_| {}, State::Ok, None, None),
            (
                "never polled: unknown, not ok",
                |s| s.reachable = None,
                State::Unknown,
                Some("not_checked"),
                Some("Not checked yet"),
            ),
            (
                "nothing measured but a URL",
                |s| {
                    *s = Signals {
                        has_url: true,
                        ..Signals::default()
                    }
                },
                State::Unknown,
                Some("not_checked"),
                None,
            ),
            (
                "never polled but the token is revoked: stopped beats unknown",
                |s| {
                    s.reachable = None;
                    s.token_valid = Some(false);
                },
                State::Stopped,
                Some("token_revoked"),
                None,
            ),
            (
                "never polled and no URL: unwatched, degraded",
                |s| {
                    s.reachable = None;
                    s.has_url = false;
                },
                State::Degraded,
                Some("unwatched"),
                None,
            ),
            (
                "token revoked",
                |s| s.token_valid = Some(false),
                State::Stopped,
                Some("token_revoked"),
                Some("Token revoked"),
            ),
            (
                "stale heartbeat, 12 min",
                |s| s.heartbeat_age = Some(Duration::minutes(12)),
                State::Degraded,
                Some("no_heartbeat"),
                Some("No heartbeat for 12 min"),
            ),
            (
                "heartbeat under the threshold",
                |s| s.heartbeat_age = Some(Duration::minutes(4)),
                State::Ok,
                None,
                None,
            ),
            (
                "heartbeat gone 3 hours",
                |s| s.heartbeat_age = Some(Duration::minutes(185)),
                State::Stopped,
                Some("no_heartbeat"),
                Some("No heartbeat for 3 h"),
            ),
            (
                "unreachable",
                |s| s.reachable = Some(false),
                State::Degraded,
                Some("unreachable"),
                Some("Not answering"),
            ),
            (
                "disk 97% full",
                |s| s.disk_free_bytes = Some(15 * GIB),
                State::Stopped,
                Some("disk_full"),
                Some("Disk 97% full"),
            ),
            (
                "disk 92% full",
                |s| s.disk_free_bytes = Some(40 * GIB),
                State::Degraded,
                Some("disk_full"),
                Some("Disk 92% full"),
            ),
            (
                "free space only, 800 MB",
                |s| {
                    s.disk_total_bytes = None;
                    s.disk_free_bytes = Some(800 * 1024 * 1024);
                },
                State::Stopped,
                Some("disk_full"),
                Some("Disk has 800 MB free"),
            ),
            (
                "sync 401",
                |s| s.last_sync_error = Some(SyncError::Unauthorized),
                State::Stopped,
                Some("sync_rejected"),
                Some("Sync unauthorized (401)"),
            ),
            (
                "sync 403",
                |s| s.last_sync_error = Some(SyncError::Forbidden),
                State::Stopped,
                Some("sync_rejected"),
                Some("Sync refused (403)"),
            ),
            (
                "sync 429",
                |s| s.last_sync_error = Some(SyncError::RateLimited),
                State::Degraded,
                Some("sync_rate_limited"),
                Some("Sync rate-limited (429)"),
            ),
            (
                "sync stuck 14 min",
                |s| s.sync_backlog_age = Some(Duration::minutes(14)),
                State::Degraded,
                Some("sync_backlog"),
                Some("Sync stuck for 14 min"),
            ),
            (
                "runner down",
                |s| s.runner_alive = Some(false),
                State::Stopped,
                Some("runner_down"),
                None,
            ),
            (
                "no URL",
                |s| s.has_url = false,
                State::Degraded,
                Some("unwatched"),
                Some("Not watched"),
            ),
            // Worst wins: a degraded heartbeat loses to a stopped disk.
            (
                "worst wins over a lower-priority number",
                |s| {
                    s.heartbeat_age = Some(Duration::minutes(12));
                    s.disk_free_bytes = Some(10 * GIB);
                },
                State::Stopped,
                Some("disk_full"),
                None,
            ),
            // Worst wins even when the degraded finding has the higher tie priority.
            (
                "stopped heartbeat beats a degraded disk",
                |s| {
                    s.heartbeat_age = Some(Duration::minutes(45));
                    s.disk_free_bytes = Some(40 * GIB);
                },
                State::Stopped,
                Some("no_heartbeat"),
                None,
            ),
            // Ties: same severity, fixed priority.
            (
                "tie: token revoked before a full disk",
                |s| {
                    s.disk_free_bytes = Some(5 * GIB);
                    s.token_valid = Some(false);
                },
                State::Stopped,
                Some("token_revoked"),
                None,
            ),
            (
                "tie: heartbeat before unreachable",
                |s| {
                    s.reachable = Some(false);
                    s.heartbeat_age = Some(Duration::minutes(12));
                },
                State::Degraded,
                Some("no_heartbeat"),
                None,
            ),
            (
                "tie: unreachable before unwatched",
                |s| {
                    s.has_url = false;
                    s.reachable = Some(false);
                },
                State::Degraded,
                Some("unreachable"),
                None,
            ),
        ];
        for (name, tweak, state, code, text) in cases {
            let mut signals = healthy();
            tweak(&mut signals);
            let health = evaluate(&signals);
            assert_eq!(health.state, *state, "{name}");
            assert_eq!(health.reason.as_ref().map(Reason::code), *code, "{name}");
            if let Some(text) = text {
                assert_eq!(health.reason.as_ref().map(Reason::text).as_deref(), Some(*text), "{name}");
            }
            // Every non-ok answer carries exactly one hint.
            assert_eq!(health.reason.is_some(), health.state != State::Ok, "{name}");
        }
    }

    #[test]
    fn the_tie_priority_is_a_total_fixed_order() {
        let all = [
            Reason::TokenRevoked,
            Reason::SyncRejected(SyncError::Unauthorized),
            Reason::RunnerDown,
            Reason::DiskFull {
                percent: Some(99),
                free_bytes: 0,
            },
            Reason::NoHeartbeat { minutes: 10 },
            Reason::Unreachable,
            Reason::SyncBacklog { minutes: 10 },
            Reason::SyncRateLimited,
            Reason::Unwatched,
            Reason::NotChecked,
        ];
        let priorities: Vec<u8> = all.iter().map(Reason::priority).collect();
        assert_eq!(priorities, (0..all.len() as u8).collect::<Vec<_>>());
        for reason in &all {
            assert!(!reason.hint().is_empty(), "{reason:?} has a hint");
        }
    }

    #[test]
    fn the_json_shape_is_state_code_reason_hint() {
        let ok = evaluate(&healthy()).to_json();
        assert_eq!(
            ok,
            serde_json::json!({"state": "ok", "code": null, "reason": null, "hint": null})
        );
        let mut signals = healthy();
        signals.token_valid = Some(false);
        assert_eq!(
            evaluate(&signals).to_json(),
            serde_json::json!({
                "state": "stopped", "code": "token_revoked",
                "reason": "Token revoked", "hint": "re-pair this machine",
            })
        );
    }
}
