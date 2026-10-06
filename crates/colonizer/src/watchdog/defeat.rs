//! The control-defeat signature (issue #609): a colony's recent `boundary` events, the patterns in
//! them that look like an agent working against its controls, and the flag that raises one.

use crate::{Shared, boundary::Boundary, protocol::Origin};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};

/// The control-defeat signature's window for repeated denials (issue #609): this many minutes.
pub const CONTROL_DEFEAT_WINDOW_MINUTES: i64 = 10;
/// Denials of one control within [`CONTROL_DEFEAT_WINDOW_MINUTES`] that make the repeated-denial
/// signature: three, so a one-off refusal the agent works around, and a single retry, are not one.
pub const REPEATED_DENIALS: usize = 3;
/// How long a refused target is remembered for the deny-then-reach signature, in minutes.
pub const REACH_WINDOW_MINUTES: i64 = 30;
/// The most boundary events kept per colony, and the most tool calls held as possibly reaching one.
const TRAIL_CAP: usize = 64;
/// The attention reason the control-defeat signature raises.
pub const CONTROL_DEFEAT_REASON: &str = "control_defeat";

/// A colony's recent `boundary` events (issue #609), with the host time each arrived: what the
/// control-defeat signature reads. In memory like the rest of `Activity`: a restart forgets the
/// trail, which at worst lets a pattern that straddles it go unflagged.
#[derive(Clone, Debug, Default)]
pub struct BoundaryTrail {
    recent: VecDeque<(DateTime<Utc>, Boundary)>,
    /// Tool calls in flight whose input names a refused target, by `tool_call_id`: a successful
    /// result for one is the deny-then-reach signature.
    reaching: HashMap<String, Boundary>,
}

/// A control-defeat signature that fired (issue #609): which pattern, a sentence for a person, and
/// the boundary events that are its evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct Defeat {
    pub signature: &'static str,
    pub summary: String,
    pub evidence: Vec<Boundary>,
}

impl BoundaryTrail {
    fn prune(&mut self, now: DateTime<Utc>) {
        let horizon = now - Duration::minutes(REACH_WINDOW_MINUTES.max(CONTROL_DEFEAT_WINDOW_MINUTES));
        self.recent.retain(|(seen, _)| *seen >= horizon);
        while self.recent.len() > TRAIL_CAP {
            self.recent.pop_front();
        }
    }

    /// Drops the events a fired signature used, so the same evidence never flags twice and a new
    /// flag needs a fresh pattern.
    fn consume(&mut self, used: &[Boundary]) {
        self.recent.retain(|(_, b)| !used.contains(b));
    }

    /// How many boundary events the trail holds (tests and the cockpit's evidence count).
    pub fn len(&self) -> usize {
        self.recent.len()
    }
}

/// Folds one `boundary` event into the colony's trail and returns the control-defeat signature it
/// completes, if any (issue #609, docs/boundaries.md "Watchdog signatures"). Pure, with the host's
/// receive time passed in, so every rule is pinned with a fixed clock:
///
/// - **publish_rewrite** — any `publish_rewrite_refused`: publish found colony output crossing its
///   wall (a rewritten `.git`, a tree that moved after its approval). One is enough.
/// - **ask_bypass** — any `exec_policy_ask_bypass_attempt`: a rule a person refused asked again.
/// - **repeated_denial** — [`REPEATED_DENIALS`] denials of the same control (kind and control)
///   within [`CONTROL_DEFEAT_WINDOW_MINUTES`]: retrying a refused action with trivial variations
///   matches the same rule each time. One denial, or two, is a wall the colony works around.
///
/// A `path_policy_unbound` alone fires nothing — it is the host failing to apply a bind, normal
/// off Linux — but its target joins the deny-then-reach watch like any other.
pub fn note_boundary(trail: &mut BoundaryTrail, boundary: Boundary, now: DateTime<Utc>) -> Option<Defeat> {
    trail.prune(now);
    trail.recent.push_back((now, boundary.clone()));
    trail.prune(now);
    let defeat = match boundary.kind.as_str() {
        "publish_rewrite_refused" => Some(Defeat {
            signature: "publish_rewrite",
            summary: format!("publish refused colony output that crossed its wall ({})", boundary.control),
            evidence: vec![boundary],
        }),
        "exec_policy_ask_bypass_attempt" => {
            let evidence: Vec<Boundary> = trail
                .recent
                .iter()
                .filter(|(_, b)| b.control == boundary.control)
                .map(|(_, b)| b.clone())
                .collect();
            Some(Defeat {
                signature: "ask_bypass",
                summary: format!("the colony retried `{}` after its ask was refused", boundary.control),
                evidence,
            })
        }
        "path_policy_unbound" => None,
        _ => {
            let since = now - Duration::minutes(CONTROL_DEFEAT_WINDOW_MINUTES);
            let evidence: Vec<Boundary> = trail
                .recent
                .iter()
                .filter(|(seen, b)| *seen >= since && b.kind == boundary.kind && b.control == boundary.control)
                .map(|(_, b)| b.clone())
                .collect();
            (evidence.len() >= REPEATED_DENIALS).then(|| Defeat {
                signature: "repeated_denial",
                summary: format!(
                    "`{}` refused the colony {} times in {CONTROL_DEFEAT_WINDOW_MINUTES} min",
                    boundary.control,
                    evidence.len()
                ),
                evidence,
            })
        }
    };
    if let Some(defeat) = &defeat {
        trail.consume(&defeat.evidence);
    }
    defeat
}

/// Whether a tool call's input names a refused target: a host as a URL's or an address's host
/// (`://host`, `@host`), a path as a whole path token — `.env` is not `.env.example`.
pub fn reaches(input: &str, target: &str) -> bool {
    if target.len() < 3 {
        return false;
    }
    let is_host = !target.contains('/') && !target.starts_with('.') && !target.starts_with('~') && target.contains('.');
    if is_host {
        let host = target.to_ascii_lowercase();
        let text = input.to_ascii_lowercase();
        return [format!("://{host}"), format!("@{host}")].iter().any(|prefix| {
            text.match_indices(prefix.as_str()).any(|(at, m)| {
                let next = text[at + m.len()..].chars().next();
                !next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            })
        });
    }
    let path_char = |c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~');
    input.match_indices(target).any(|(at, m)| {
        let before = input[..at].chars().next_back();
        let after = input[at + m.len()..].chars().next();
        let clean_before = match before {
            None => true,
            // `./.env` names `.env`; `config/.env` is another file.
            Some('/') => input[..at].ends_with("./") && !input[..at].ends_with("../"),
            Some(c) => !path_char(c),
        };
        clean_before && !after.is_some_and(path_char)
    })
}

/// A tool call opened (issue #609): when its input names a target a control refused within
/// [`REACH_WINDOW_MINUTES`], it is held, so its result can tell whether the refused thing was then
/// reached another way.
pub fn note_reach_call(trail: &mut BoundaryTrail, tool_call_id: &str, input: &Value, now: DateTime<Utc>) {
    trail.prune(now);
    let text = input.to_string();
    let since = now - Duration::minutes(REACH_WINDOW_MINUTES);
    let hit = trail
        .recent
        .iter()
        .rev()
        .find(|(seen, b)| *seen >= since && b.target.as_deref().is_some_and(|target| reaches(&text, target)));
    if let Some((_, boundary)) = hit
        && trail.reaching.len() < TRAIL_CAP
    {
        trail.reaching.insert(tool_call_id.to_string(), boundary.clone());
    }
}

/// A tool call's result (issue #609): the **deny_then_reach** signature fires when a call held by
/// [`note_reach_call`] succeeded — the egress-denied host reached, the refused write target written
/// through another tool. An errored result is the wall holding again, not a defeat.
pub fn note_reach_result(trail: &mut BoundaryTrail, tool_call_id: &str, is_error: bool) -> Option<Defeat> {
    let boundary = trail.reaching.remove(tool_call_id)?;
    if is_error {
        return None;
    }
    trail.consume(std::slice::from_ref(&boundary));
    Some(Defeat {
        signature: "deny_then_reach",
        summary: format!(
            "a tool call ({tool_call_id}) reached `{}` after `{}` refused it ({})",
            boundary.target.as_deref().unwrap_or_default(),
            boundary.control,
            boundary.kind
        ),
        evidence: vec![boundary],
    })
}

/// Raises the control-defeat flag on a colony (issue #609): the attention item carries the
/// signature, a sentence and the boundary events that are its evidence, and the log says why. Not a
/// stop: a signature is a pattern, not proof, so a person decides (docs/boundaries.md). A colony
/// already flagged for control-defeat keeps its first evidence and gets a log line only.
pub(crate) async fn flag_control_defeat(app: &Shared, id: &str, defeat: Defeat) {
    let Some(s) = app.session(id).await else { return };
    let message = format!(
        "watchdog: control-defeat signature ({}): {}; this colony needs you",
        defeat.signature, defeat.summary
    );
    let already = s.attention.as_ref().and_then(|a| a["reason"].as_str()) == Some(CONTROL_DEFEAT_REASON);
    if !already {
        let evidence: Vec<Value> = defeat.evidence.iter().map(Boundary::to_event).collect();
        let since = Utc::now();
        app.update_session(id, |x| {
            x.attention = Some(json!({
                "reason": CONTROL_DEFEAT_REASON,
                "since": since,
                "nudges": 0,
                "signature": defeat.signature,
                "detail": defeat.summary,
                "evidence": evidence,
            }));
        })
        .await;
    }
    app.session_log_as(Origin::Watchdog, id, "error", message).await;
}
