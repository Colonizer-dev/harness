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

mod defeat;
pub(crate) use defeat::flag_control_defeat;
pub use defeat::{BoundaryTrail, CONTROL_DEFEAT_REASON, note_boundary, note_reach_call, note_reach_result};

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
mod tests;
