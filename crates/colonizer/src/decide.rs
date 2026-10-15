//! The shared Jev decision layer (issue #582): one place a point in the harness asks the optional
//! external classifier for a pick, however many points there come to be. A point names a question,
//! the closed set of options it may answer with, and a time budget; the ask produces a [`Decision`]
//! or a [`Miss`] saying why there was none. There is no free text in either direction — Jev returns a
//! choice and a confidence, never a reason — so nothing a point does can be talked into an action by
//! prose, and nothing here is ever told a secret: a point's context is metadata it built itself.
//!
//! Three hard limits ride along with every ask: a pick outside the point's declared options is a
//! [`Miss::OutsideOptions`] and never acted on; a point whose id begins `publish.`, `security.`,
//! `delete.` or `destroy.` is refused before any network call; and every ask is bounded by its
//! point's budget, running past which is [`Miss::Timeout`] and leaves the caller on its own rule.
//!
//! The three [`Mode`]s are the routing point's: `off` (nothing asked, the default), `shadow` (asked
//! and recorded, never applied) and `act` (the pick is used when it is confident enough). Every ask
//! that is not off appends one `decisions.jsonl` row and one activity line via [`record`], so a
//! report can tell a point that never asked from one whose answers are being ignored.

use crate::App;
use crate::jev::{AskError, JevClient};
use crate::sessions::Session;
use crate::util::append_line;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, Instant};

/// What the harness does with Jev's opinion at a point. `Off` asks nothing; `Shadow` asks and
/// records the answer without ever applying it; `Act` applies the pick when it is confident enough.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Not asked.
    #[default]
    Off,
    /// Asked and recorded, never applied.
    Shadow,
    /// Asked, and its pick is used when it is confident enough — never below the point's floor.
    Act,
}

impl Mode {
    /// The mode from the two module settings: `act` wins, then `shadow`.
    pub fn from_settings(act: bool, shadow: bool) -> Mode {
        if act {
            Mode::Act
        } else if shadow {
            Mode::Shadow
        } else {
            Mode::Off
        }
    }

    /// Whether the harness should ask Jev at all: act needs the answer just as shadow does.
    pub fn asks(&self) -> bool {
        *self != Mode::Off
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Shadow => "shadow",
            Mode::Act => "act",
        }
    }
}

/// One place the harness may ask Jev: the id it records under, the question it asks, and the ceiling
/// on one ask — the whole ask, retries and backoff included, after which the point falls back to its
/// rule.
#[derive(Clone, Copy, Debug)]
pub struct Point {
    /// The point's stable id, e.g. `routing.tier`: the ledger name, and what the forbidden-prefix
    /// check reads.
    pub id: &'static str,
    /// The Jev question id this point asks (always a `choice` question).
    pub question: &'static str,
    /// The most time one ask may take before it is a [`Miss::Timeout`].
    pub budget: Duration,
}

/// The model-tier routing point (routing.rs): the question and the 1800 ms ceiling the pipeline has
/// always used.
pub const ROUTING_TIER: Point = Point {
    id: "routing.tier",
    question: "tier",
    budget: Duration::from_millis(1800),
};

/// The recovery-path point (recovery.rs): what to do when a step fails — a provider error, a tool
/// failure, a watchdog stall or an `autopilot_held`. Its options are built per failure class by
/// `recovery::options`, and the point is the same 1800 ms ceiling the routing point uses.
pub const RECOVERY_PATH: Point = Point {
    id: "recovery.path",
    question: "recovery",
    budget: Duration::from_millis(1800),
};

/// The verifier's focused-check point (verify_focus.rs, #584): which focused check the verifier
/// runs first, before the full suite. Its options are built per diff by `verify_focus::focus_candidates`
/// plus `full` for "full suite only". Asked in shadow only — the pick is recorded and graded against
/// what the checks found, never applied, and the same 1800 ms ceiling the other points use.
pub const VERIFY_FOCUS: Point = Point {
    id: "verify.focus",
    question: "verify_focus",
    budget: Duration::from_millis(1800),
};

/// Point-id prefixes the harness refuses outright: a destructive, security or publish decision is
/// never handed to an external classifier, whatever a future point is called. Checked in [`decide`]
/// before any network call, so a refused point costs nothing.
const FORBIDDEN: [&str; 4] = ["publish.", "security.", "delete.", "destroy."];

/// Whether a point id names a decision the harness will not hand to Jev.
fn forbidden(point_id: &str) -> bool {
    FORBIDDEN.iter().any(|prefix| point_id.starts_with(prefix))
}

/// A pick Jev made, with what it took and what it cost. The confidence is the vendor's own, read
/// back unmodified; whether it clears a point's threshold is the caller's call, not this layer's.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Decision {
    /// The chosen option — always one of the options the point passed in.
    pub pick: String,
    /// The model that answered, pinned by [`crate::jev::JEV_MODEL`].
    pub model: String,
    /// The vendor's confidence in the winner, from 0 to 1.
    pub confidence: f64,
    /// How long the ask took, wall time.
    pub latency_ms: u64,
    /// A rough cost estimate for the ask, not metered billing (see `jev.rs`).
    pub estimated_cost_usd: f64,
}

/// Why a point produced no pick. Every one leaves the caller on its own rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Miss {
    /// The point's mode is off: nothing was asked.
    Off,
    /// The org switched Jev off for its colonies.
    OrgOff,
    /// There is no usable `JEV_API_KEY` secret.
    NoKey,
    /// The point id names a decision the harness never hands to Jev.
    Forbidden,
    /// The ask did not finish inside the point's budget.
    Timeout,
    /// The ask failed: a non-2xx status, a network error, or a response that would not parse.
    Error,
    /// Jev picked something outside the options the point offered, so nothing can be acted on.
    OutsideOptions,
}

impl Miss {
    /// The miss's name as it is spelled in the ledger and log lines.
    pub fn as_str(&self) -> &'static str {
        match self {
            Miss::Off => "off",
            Miss::OrgOff => "org_off",
            Miss::NoKey => "no_key",
            Miss::Forbidden => "forbidden",
            Miss::Timeout => "timeout",
            Miss::Error => "error",
            Miss::OutsideOptions => "outside_options",
        }
    }
}

/// Asks Jev one point's question on the real endpoint and key. The short-circuits run in the order
/// of the guarantees — off, refused point, org switch, missing key — and all four make zero network
/// calls, so a point that is not going to be answered never opens a connection.
pub async fn decide<C: Serialize>(
    point: &Point,
    mode: Mode,
    org_allows: bool,
    options: &[&str],
    context: &C,
) -> Result<Decision, Miss> {
    decide_at(
        point,
        mode,
        org_allows,
        options,
        context,
        crate::jev::api_key(),
        crate::jev::ENDPOINT.to_string(),
    )
    .await
}

/// [`decide`] for a caller that carries its `App` along: the same ask on the same endpoint and key —
/// except in a test build, where the `App` under test can carry a redirect to a loopback mock
/// (`App::test_ask`), so an integration test can answer a point without touching the process
/// environment or the secrets store, and without any other test's ask seeing its mock.
pub(crate) async fn decide_for<C: Serialize>(
    app: &App,
    point: &Point,
    mode: Mode,
    org_allows: bool,
    options: &[&str],
    context: &C,
) -> Result<Decision, Miss> {
    #[cfg(test)]
    let redirected = app.test_ask.lock().expect("test ask lock").clone();
    #[cfg(test)]
    if let Some((base_url, api_key)) = redirected {
        return decide_at(point, mode, org_allows, options, context, Some(api_key), base_url).await;
    }
    let _ = app; // release builds carry no redirect, so the app is otherwise unread
    decide(point, mode, org_allows, options, context).await
}

/// [`decide`] with the key and endpoint passed in, so a test can point it at a loopback mock and
/// choose whether a key exists.
async fn decide_at<C: Serialize>(
    point: &Point,
    mode: Mode,
    org_allows: bool,
    options: &[&str],
    context: &C,
    api_key: Option<String>,
    base_url: String,
) -> Result<Decision, Miss> {
    if mode == Mode::Off {
        return Err(Miss::Off);
    }
    if forbidden(point.id) {
        return Err(Miss::Forbidden);
    }
    if !org_allows {
        return Err(Miss::OrgOff);
    }
    let Some(api_key) = api_key else {
        return Err(Miss::NoKey);
    };
    let started = Instant::now();
    let client = JevClient::new_at(api_key, base_url);
    match client.ask(point.question, options, context, point.budget).await {
        Ok(answer) => {
            // An answer outside the options is not a pick at all: a point may only ever be answered
            // with something it offered. Matched the way the routing point's tier parse always has —
            // trimmed, case-insensitive — so a reply's spelling alone cannot turn a real pick into a
            // miss; the pick stored is the option as the point declared it.
            let Some(pick) = options
                .iter()
                .find(|option| option.eq_ignore_ascii_case(answer.choice.trim()))
            else {
                return Err(Miss::OutsideOptions);
            };
            Ok(Decision {
                pick: (*pick).to_string(),
                model: answer.model,
                confidence: answer.confidence,
                latency_ms: started.elapsed().as_millis() as u64,
                estimated_cost_usd: answer.estimated_cost_usd,
            })
        }
        Err(AskError::Timeout) => Err(Miss::Timeout),
        Err(AskError::Error) => Err(Miss::Error),
    }
}

/// One `decisions.jsonl` row: what one point asked, what came back, and what the harness did about
/// it. `did` is the point's own word for the end state — routing writes `jev` when the answer was
/// applied and `rule` otherwise. `outcome` grades a decision against what actually happened; only
/// the recovery point (a second, `kind: "outcome"` row) and the verifier's focused-check point (in
/// the decision row itself) fill it today, so it is null wherever a point does not grade.
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub kind: &'static str,
    pub ts: DateTime<Utc>,
    pub point: &'static str,
    pub session: String,
    pub repo: String,
    pub issue: Option<u64>,
    pub mode: Mode,
    pub options: Vec<String>,
    /// The pick, when there was one.
    pub pick: Option<String>,
    pub confidence: Option<f64>,
    /// The ask's wall time; zero on a miss, where no decision was produced to time.
    pub latency_ms: u64,
    /// Why there was no pick, when there was none.
    pub miss: Option<Miss>,
    /// What the harness did in the end: the point's word for it (`jev` or `rule` for routing;
    /// `rule`, `jev` or `cap` for recovery).
    pub did: &'static str,
    /// How the decision actually turned out, graded by the point that fills it (recovery writes it
    /// to a later `kind: "outcome"` row; the verifier's focused-check point fills it in the decision
    /// row itself). Null unless a point grades.
    pub outcome: Option<Value>,
}

/// Builds the ledger row for one point's ask. Pure, so its shape can be tested apart from the file.
pub fn row(
    point: &Point,
    session: &Session,
    mode: Mode,
    options: &[&str],
    result: &Result<Decision, Miss>,
    did: &'static str,
) -> Row {
    let (pick, confidence, latency_ms, miss) = match result {
        Ok(decision) => (
            Some(decision.pick.clone()),
            Some(decision.confidence),
            decision.latency_ms,
            None,
        ),
        Err(miss) => (None, None, 0, Some(*miss)),
    };
    Row {
        kind: "decision",
        ts: Utc::now(),
        point: point.id,
        session: session.id.clone(),
        repo: session.repo.clone(),
        issue: session.issue,
        mode,
        options: options.iter().map(|option| (*option).to_string()).collect(),
        pick,
        confidence,
        latency_ms,
        miss,
        did,
        outcome: None,
    }
}

/// Records one point's ask: the row is appended to `decisions.jsonl` and an activity line written,
/// both best-effort — a lost record is a lost measurement, not a failed boot, so a failed append only
/// raises the storage alert (the routing ledger's deal).
pub async fn record(app: &App, row: &Row) {
    if let Ok(line) = serde_json::to_string(row)
        && let Err(e) = append_line(&app.decisions_file(), &line).await
    {
        app.storage_failed("append to the decisions ledger", &e).await;
    }
    record_activity(app, row).await;
}

/// The activity line for one ask, kinded by what the harness did with it: `decision.shadow` for a
/// shadow ask, `decision.act` for an act ask whose pick was used, and `decision.fallback` for
/// anything else. The actor is the colony, since the decision is the harness's own, not a person's.
async fn record_activity(app: &App, row: &Row) {
    let kind = match (row.mode, row.did) {
        (Mode::Shadow, _) => "decision.shadow",
        (Mode::Act, "jev") => "decision.act",
        _ => "decision.fallback",
    };
    let mut entry = crate::activity::Entry::new(kind, "colony");
    entry.colony = Some(row.session.clone());
    entry.repo = Some(row.repo.clone()).filter(|repo| !repo.is_empty());
    entry.org = entry
        .repo
        .as_deref()
        .and_then(|repo| repo.split_once('/'))
        .map(|(owner, _)| owner.to_string());
    entry.issue = row.issue;
    entry.detail = Some(match (&row.pick, row.miss) {
        (Some(pick), _) => format!("{}: picked {pick}", row.point),
        (None, Some(miss)) => format!("{}: no pick ({})", row.point, miss.as_str()),
        (None, None) => format!("{}: no pick", row.point),
    });
    crate::activity::record(app, entry).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jev::mock::{Reply, serve};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OPTIONS: [&str; 3] = ["low", "medium", "high"];

    fn session() -> Session {
        Session {
            id: "abc".into(),
            repo: "acme/web".into(),
            issue: Some(12),
            ..Session::default()
        }
    }

    fn requests(count: &Arc<AtomicUsize>) -> usize {
        count.load(Ordering::SeqCst)
    }

    /// Every off/refused short-circuit makes zero requests even when the point is given a key and a
    /// reachable endpoint.
    #[tokio::test]
    async fn the_point_stays_off_and_asks_nothing_without_a_key_or_when_the_org_is_off() {
        let (base, count) = serve(Reply::Choice("high")).await;
        let context = ();

        // Mode off: nothing asked even with a key and a live endpoint.
        let miss = decide_at(
            &ROUTING_TIER,
            Mode::Off,
            true,
            &OPTIONS,
            &context,
            Some("key".into()),
            base.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(miss, Miss::Off);

        // Shadow with no key: nothing asked.
        let miss = decide_at(&ROUTING_TIER, Mode::Shadow, true, &OPTIONS, &context, None, base.clone())
            .await
            .unwrap_err();
        assert_eq!(miss, Miss::NoKey);

        // The org switched Jev off: nothing asked.
        let miss = decide_at(
            &ROUTING_TIER,
            Mode::Shadow,
            false,
            &OPTIONS,
            &context,
            Some("key".into()),
            base.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(miss, Miss::OrgOff);

        // A point the harness never hands to Jev: nothing asked.
        let forbidden_point = Point {
            id: "publish.pr",
            question: "tier",
            budget: Duration::from_millis(1800),
        };
        let miss = decide_at(
            &forbidden_point,
            Mode::Act,
            true,
            &OPTIONS,
            &context,
            Some("key".into()),
            base,
        )
        .await
        .unwrap_err();
        assert_eq!(miss, Miss::Forbidden);

        assert_eq!(requests(&count), 0, "no short-circuit may reach the endpoint");

        // Each forbidden prefix refuses by id, and an ordinary point name never does.
        for id in ["publish.pr", "security.scan", "delete.worktree", "destroy.vm"] {
            assert!(forbidden(id));
        }
        assert!(!forbidden("routing.tier") && !forbidden("summarize.issue"));
    }

    /// Every way an ask can come back without a usable pick is a miss, never an action: an answer
    /// outside the options, a server that never answers, a 5xx.
    #[tokio::test]
    async fn a_bad_or_out_of_bounds_ask_is_a_miss_never_an_action() {
        for (reply, expected) in [
            (Reply::Choice("expensive"), Miss::OutsideOptions),
            (Reply::NeverRespond, Miss::Timeout),
            (Reply::Status(500), Miss::Error),
        ] {
            let (base, _) = serve(reply).await;
            let started = Instant::now();
            let miss = decide_at(&ROUTING_TIER, Mode::Act, true, &OPTIONS, &(), Some("key".into()), base)
                .await
                .unwrap_err();
            assert_eq!(miss, expected);
            if expected == Miss::Timeout {
                assert!(started.elapsed() < ROUTING_TIER.budget, "an ask must not outlive its budget");
            }
        }
    }

    #[tokio::test]
    async fn a_valid_answer_becomes_a_decision_with_its_pick_and_confidence() {
        let (base, _) = serve(Reply::Choice("high")).await;
        let decision = decide_at(&ROUTING_TIER, Mode::Act, true, &OPTIONS, &(), Some("key".into()), base)
            .await
            .expect("a valid answer is a decision");
        assert_eq!(decision.pick, "high");
        assert_eq!(decision.confidence, 0.87);
        assert_eq!(decision.model, crate::jev::JEV_MODEL);
        assert!(decision.estimated_cost_usd > 0.0);

        // A reply that spells the option differently still names it, matched as the routing point's
        // tier parse always has: only a genuinely different word is a miss.
        let (base, _) = serve(Reply::Choice(" HIGH ")).await;
        let decision = decide_at(&ROUTING_TIER, Mode::Act, true, &OPTIONS, &(), Some("key".into()), base)
            .await
            .expect("a differently-spelled option is still the option");
        assert_eq!(decision.pick, "high");
    }

    /// A row carries the pick or the miss and what the harness did with it: `rule` for a shadow-mode
    /// miss (nothing applied), `jev` for an applied act pick, with the ask's latency.
    #[test]
    fn a_row_carries_the_pick_or_the_miss_and_what_the_harness_did() {
        let miss = row(&ROUTING_TIER, &session(), Mode::Shadow, &OPTIONS, &Err(Miss::NoKey), "rule");
        assert_eq!(miss.kind, "decision");
        assert_eq!(miss.point, "routing.tier");
        assert_eq!(miss.session, "abc");
        assert_eq!(miss.repo, "acme/web");
        assert_eq!(miss.issue, Some(12));
        assert_eq!(miss.mode, Mode::Shadow);
        assert_eq!(miss.options, vec!["low", "medium", "high"]);
        assert_eq!(miss.pick, None);
        assert_eq!(miss.miss, Some(Miss::NoKey));
        assert_eq!(miss.did, "rule");
        assert_eq!(miss.outcome, None);
        let value: Value = serde_json::to_value(&miss).unwrap();
        assert_eq!(value["miss"], "no_key");
        assert_eq!(value["mode"], "shadow");
        assert_eq!(value["did"], "rule");
        assert!(value["pick"].is_null());

        let decision = Decision {
            pick: "high".into(),
            model: crate::jev::JEV_MODEL.into(),
            confidence: 0.9,
            latency_ms: 42,
            estimated_cost_usd: 0.0001,
        };
        let picked = row(&ROUTING_TIER, &session(), Mode::Act, &OPTIONS, &Ok(decision), "jev");
        assert_eq!(picked.pick.as_deref(), Some("high"));
        assert_eq!(picked.confidence, Some(0.9));
        assert_eq!(picked.latency_ms, 42);
        assert_eq!(picked.miss, None);
        assert_eq!(picked.did, "jev");
    }
}
