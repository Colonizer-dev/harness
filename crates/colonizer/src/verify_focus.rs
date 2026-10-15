//! Issue #584: focused checks before the full suite. When a diff owes more than one check, the
//! check owning most of the changed files is the likeliest to catch a regression, and running it
//! first can stop a doomed verification early. The `publish` module's `verify_focus` setting picks
//! the mode: `off` runs the checks in the diff's order, as before; `shadow` (the default) runs them
//! exactly as `off` does and only measures what focused-first would have done; `act` runs the
//! chosen check first and stops at its contradiction. Whatever the mode, a confirmed verdict still
//! needs every selected check run green — focusing reorders the suite, it never shortens it.
//!
//! Shadow and act append one row per verification that ran checks to the data-dir-wide
//! `jev_focus.jsonl` (outliving per-colony cleanup like `jev_ladder.jsonl`): the candidates, the
//! choice, whether it would have caught the failure, and time-to-first-failure with and without.
//! Beside the rule, the decision layer (#582) asks Jev the same question at the `verify.focus`
//! point — shadow only, its answer recorded to `decisions.jsonl` with the same grading, never
//! applied ([`ask`]).

use crate::decide;
use crate::sessions::Session;
use crate::{App, Shared, util::append_line, verify::Check, verify::Verdict};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    Off,
    Shadow,
    Act,
}

/// The `publish` module's `verify_focus` setting, read at verify time.
pub(crate) async fn mode(app: &App) -> Mode {
    let modules = app.modules.read().await.clone();
    let schema = crate::modules::schema_for("publish", &modules.publish.provider, &app.agents);
    match crate::config::setting_str(&modules.publish, &schema, "verify_focus").as_str() {
        "off" => Mode::Off,
        "act" => Mode::Act,
        _ => Mode::Shadow,
    }
}

/// One focused set the diff offers: a selected check, and how many of the changed files it owns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Candidate {
    pub label: String,
    pub owned: usize,
}

/// The focused sets to choose among, one per check, in the checks' order; "full suite only" is
/// implicit. With fewer than two checks the focused set is the full suite, so there is nothing.
/// A changed file is owned by the check whose directory is its nearest ancestor (the first such
/// check on a tie; the root check owns what no deeper one does).
pub(crate) fn focus_candidates(checks: &[Check], changed: &[String]) -> Vec<Candidate> {
    if checks.len() < 2 {
        return Vec::new();
    }
    let mut owned = vec![0; checks.len()];
    for path in changed {
        let under = |dir: &str| dir.is_empty() || path.starts_with(&format!("{dir}/"));
        let owner = checks
            .iter()
            .enumerate()
            .filter(|(_, c)| under(&c.dir))
            .min_by_key(|(_, c)| std::cmp::Reverse(c.dir.len()));
        if let Some((i, _)) = owner {
            owned[i] += 1;
        }
    }
    checks
        .iter()
        .zip(owned)
        .map(|(c, owned)| Candidate {
            label: format!("{}: {}", if c.dir.is_empty() { "." } else { &c.dir }, c.command),
            owned,
        })
        .collect()
}

/// Which candidate runs first, or `None` for "full suite only". The deterministic rule the verifier
/// acts on: the check owning the most changed files, the first on a tie. Jev is asked the same
/// question beside it, in shadow ([`ask`]) — its pick is recorded and graded, never applied. A
/// confident pick taking over the order would be #582's act mode, measured first.
pub(crate) fn choose(candidates: &[Candidate]) -> Option<usize> {
    let best = candidates.iter().map(|c| c.owned).max().filter(|&n| n > 0)?;
    candidates.iter().position(|c| c.owned == best)
}

/// One check as it ran: its index in the selected checks, its wall time (head run plus any base
/// rerun), and whether it contradicted the claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub check: usize,
    pub ms: u64,
    pub failed: bool,
}

/// Milliseconds up to and including the first failing run, in the given order.
fn first_failure_ms<'a>(runs: impl Iterator<Item = &'a Run>) -> Option<u64> {
    let mut total = 0;
    for run in runs {
        total += run.ms;
        if run.failed {
            return Some(total);
        }
    }
    None
}

/// What focused-first did (or would have done) against the runs that happened: whether the chosen
/// check catches the failure (`None` when nothing failed), the actual time-to-first-failure, and
/// the focused one — the chosen check first, the rest in the order they ran.
pub(crate) fn measure(chosen: Option<usize>, runs: &[Run]) -> (Option<bool>, Option<u64>, Option<u64>) {
    let actual = first_failure_ms(runs.iter());
    let would_catch = actual.map(|_| runs.iter().any(|r| Some(r.check) == chosen && r.failed));
    let focused = first_failure_ms(
        runs.iter()
            .filter(|r| Some(r.check) == chosen)
            .chain(runs.iter().filter(|r| Some(r.check) != chosen)),
    );
    (would_catch, actual, focused)
}

/// One `jev_focus.jsonl` row, kind-tagged like the other Jev ledgers.
#[derive(Debug, Serialize)]
struct FocusRow<'a> {
    kind: &'static str,
    ts: chrono::DateTime<chrono::Utc>,
    session: &'a str,
    mode: Mode,
    candidates: &'a [Candidate],
    /// The chosen candidate's label, or `full` for "full suite only".
    chosen: &'a str,
    would_catch: Option<bool>,
    verdict: Verdict,
    actual_first_failure_ms: Option<u64>,
    focused_first_failure_ms: Option<u64>,
    total_ms: u64,
    checks_run: usize,
}

/// Records one verification's focus measurement: a ledger row and a line in the colony's log.
/// A lost row is a lost measurement, never a failed verification.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record(
    app: &App,
    session: &str,
    mode: Mode,
    candidates: &[Candidate],
    chosen: Option<usize>,
    runs: &[Run],
    verdict: Verdict,
    total_ms: u64,
) {
    let (would_catch, actual, focused) = measure(chosen, runs);
    let label = chosen.map_or("full", |i| candidates[i].label.as_str());
    let row = FocusRow {
        kind: "focus",
        ts: chrono::Utc::now(),
        session,
        mode,
        candidates,
        chosen: label,
        would_catch,
        verdict,
        actual_first_failure_ms: actual,
        focused_first_failure_ms: focused,
        total_ms,
        checks_run: runs.len(),
    };
    if let Ok(line) = serde_json::to_string(&row)
        && let Err(e) = append_line(&app.jev_focus_file(), &line).await
    {
        app.storage_failed("append to the jev focus ledger", &e).await;
    }
    let ms = |v: Option<u64>| v.map_or("none".to_string(), |ms| format!("{ms} ms"));
    let caught = match would_catch {
        None => "nothing failed".to_string(),
        Some(c) => format!("would have caught the failure: {c}"),
    };
    let ran = if mode == Mode::Act { "ran" } else { "would run" };
    app.session_log(
        session,
        "info",
        format!(
            "verify focus ({}): {ran} `{label}` first; {caught}; first failure {} actual, {} focused; {} checks in {total_ms} ms",
            if mode == Mode::Act { "act" } else { "shadow" },
            ms(actual),
            ms(focused),
            runs.len()
        ),
    )
    .await;
}

/// Asks the `verify.focus` decision point (#584) which focused check Jev would run first, in shadow:
/// the ask goes through the shared layer in `decide.rs` with the candidate labels plus `full`, and
/// the pick is graded against the runs that happened — never applied. Detached, so the verification
/// pays nothing for it; the ask is best-effort, a lost one a lost measurement, never a failed
/// verification.
pub(crate) fn ask(app: Shared, session: Session, candidates: &[Candidate], changed: &[String], runs: &[Run], total_ms: u64) {
    let candidates = candidates.to_vec();
    let changed = changed.to_vec();
    let runs = runs.to_vec();
    tokio::spawn(async move {
        let mut options: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
        options.push("full");
        // Metadata only, built by the harness: how many files the diff touches, their extensions,
        // and per candidate how many of those files it owns.
        let mut extensions: BTreeMap<&str, usize> = BTreeMap::new();
        for path in &changed {
            if let Some((_, ext)) = path.rsplit_once('.') {
                *extensions.entry(ext).or_default() += 1;
            }
        }
        let context = json!({
            "changed_files": changed.len(),
            "extensions": extensions,
            "candidates": candidates
                .iter()
                .map(|c| json!({"check": c.label, "changed_files": c.owned}))
                .collect::<Vec<_>>(),
        });
        let org_allows = app.org_settings(&session.org).jev != Some(false);
        let result = decide::decide_for(
            &app,
            &decide::VERIFY_FOCUS,
            decide::Mode::Shadow,
            org_allows,
            &options,
            &context,
        )
        .await;
        // The pick is stored spelled exactly as an option (the layer matches a reply's spelling and
        // keeps the declared one), so mapping it back is an exact match; `full` is no pick.
        let pick = match result.as_ref().ok().map(|d| d.pick.as_str()) {
            Some("full") | None => None,
            Some(label) => candidates.iter().position(|c| c.label == label),
        };
        let (would_catch, actual, focused) = measure(pick, &runs);
        let mut row = decide::row(
            &decide::VERIFY_FOCUS,
            &session,
            decide::Mode::Shadow,
            &options,
            &result,
            "rule",
        );
        if result.is_ok() {
            row.outcome = Some(json!({
                "would_catch": would_catch,
                "actual_first_failure_ms": actual,
                "focused_first_failure_ms": focused,
                "total_ms": total_ms,
            }));
        }
        decide::record(&app, &row).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(dir: &str) -> Check {
        Check {
            dir: dir.into(),
            command: "cargo test".into(),
            source: "Cargo.toml",
            needs: None,
            runs_script: false,
        }
    }

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    /// The check owning most changed files is chosen, the first on a tie; one check is the full
    /// suite already, and the root owns only what no deeper check does.
    #[test]
    fn the_check_owning_most_changed_files_goes_first() {
        let checks = [check("a"), check("b"), check("")];
        let changed = files(&["a/x.rs", "b/y.rs", "b/z.rs", "README.md", "ab/w.rs"]);
        let c = focus_candidates(&checks, &changed);
        assert_eq!(
            c.iter().map(|c| (c.label.as_str(), c.owned)).collect::<Vec<_>>(),
            [("a: cargo test", 1), ("b: cargo test", 2), (".: cargo test", 2)]
        );
        assert_eq!(choose(&c), Some(1), "b beats the root on the tie by coming first");
        assert_eq!(
            choose(&focus_candidates(&checks[..2], &files(&["a/x.rs", "b/y.rs"]))),
            Some(0)
        );
        assert!(
            focus_candidates(&checks[..1], &changed).is_empty(),
            "one check is the full suite"
        );
        assert_eq!(choose(&[]), None);
    }

    /// Time-to-first-failure with and without focus, and whether the choice catches it.
    #[test]
    fn the_measurement_compares_focused_with_actual_order() {
        let run = |check, ms, failed| Run { check, ms, failed };
        let runs = [run(0, 100, false), run(1, 50, true), run(2, 30, false)];
        assert_eq!(measure(Some(1), &runs), (Some(true), Some(150), Some(50)));
        assert_eq!(measure(Some(2), &runs), (Some(false), Some(150), Some(180)));
        assert_eq!(measure(None, &runs), (Some(false), Some(150), Some(150)));
        let green = [run(0, 100, false), run(1, 50, false)];
        assert_eq!(measure(Some(1), &green), (None, None, None));
    }
}
