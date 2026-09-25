//! Jev visibility ladder, stage 1 — measure (#475): the harness's half of grading what Jev
//! compaction actually did to a transcript. When a compaction pass runs, the runner reports every
//! chunk's keep/drop decision with the plugin's own relevance scores on a `jev_ladder` event; this
//! module appends one `decision` row per chunk to the data-dir-wide `jev_ladder.jsonl`, and watches
//! for the agent re-issuing an equivalent tool call later in the same session — the ground truth
//! that a chunk the pass touched was actually needed again. Each such match appends one `reread`
//! row, and the colony's log carries the running precision/recall of the decisions against those
//! rereads.
//!
//! Shadow measurement only: nothing here changes what compaction keeps or drops, and nothing reads
//! the ledger to make a decision. The ledger lives in the data dir, outliving per-colony cleanup
//! the way `routing.jsonl` does, because stage 2's bench-wide report grades colonies against each
//! other. Known limitation: a pass the plugin computed but did not apply (`applied: false`, the
//! fallback path) is not measured — nothing was removed from the transcript, so there is nothing
//! for a later call to be a reread of.
//!
//! The match is deliberately coarse: same tool name, same canonical input, nothing smarter.
//! Provenance is not distinguished (an orchestrator call and a subagent's with equal arguments are
//! interchangeable), and only exact arguments count as equivalent.

use crate::{Shared, protocol::JevDecision, sessions::Runtime, util::append_line};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, collections::VecDeque, path::Path, sync::Arc};

/// A decision's `keep_result` at or above this counts as "the plugin predicted this chunk would be
/// needed" — the same default the plugin itself keeps at (`jev_keep_threshold`, docs/protocol.md,
/// Token savings). The boundary is `>=` on purpose: a score exactly at the threshold kept the chunk,
/// so it predicted positively.
pub(crate) const PREDICTED_THRESHOLD: f64 = 0.5;

/// How many recent tool calls keep their name and input signature in memory for decision-time
/// lookup. A pass grades chunks from the recent context, so a decision's original is almost always
/// still there; an evicted one still gets its `decision` row, it just cannot arm the reread watch —
/// a lost measurement, never a wrong one.
const CALL_MEMORY: usize = 512;

/// One JSONL row of `jev_ladder.jsonl`, tagged `kind` exactly like the routing ledger's rows, so a
/// reader can interleave or split the two row kinds by the same convention.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LadderRow {
    /// One chunk's decision in one applied compaction pass.
    Decision {
        ts: DateTime<Utc>,
        session: String,
        tool_call_id: String,
        tool: String,
        /// The wire spelling (`keep`/`drop_result`/`drop_call`, `unknown` for an action this build
        /// does not know).
        action: String,
        keep_call: Option<f64>,
        keep_result: Option<f64>,
    },
    /// A later tool call that re-issued an original a pending decision was watching.
    Reread {
        ts: DateTime<Utc>,
        session: String,
        /// The new call's id.
        tool_call_id: String,
        /// The original the decision was about.
        matched_tool_call_id: String,
        tool: String,
    },
}

impl LadderRow {
    /// The JSONL line, one row per append. A row is plain data, so serialising cannot fail; the
    /// caller skips a `None` rather than turning a measurement into an error (the `activity.rs`
    /// convention).
    fn line(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }
}

/// Precision and recall of the compaction's keep/drop decisions against the reread ground truth.
/// A denominator of zero is `None`, not `0.0` or NaN: precision over no positive predictions and
/// recall over no actual positives are undefined, and a report must not read them as a bad score.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub(crate) struct LadderMetrics {
    /// Predicted needed (kept, `keep_result >= threshold`) and actually re-issued.
    pub(crate) tp: usize,
    /// Predicted needed but never re-issued.
    pub(crate) fp: usize,
    /// Predicted not needed but re-issued — the drop the agent worked around.
    pub(crate) fn_count: usize,
    /// Predicted not needed and never re-issued.
    pub(crate) tn: usize,
    pub(crate) precision: Option<f64>,
    pub(crate) recall: Option<f64>,
}

/// Grades ledger rows against `threshold`. Predicted-positive is a `Decision` row whose
/// `keep_result` is present and at or above `threshold` (an unscored chunk predicts nothing);
/// actual-positive is any `Reread` row whose `matched_tool_call_id` names that decision's
/// `tool_call_id`. Everything else in the ledger — rereads with no decision, rows of other
/// sessions — contributes only through its decision. The double pass is O(n²); the ledger is one
/// row per chunk per pass, written rarely, read per reread.
pub(crate) fn precision_recall(rows: &[LadderRow], threshold: f64) -> LadderMetrics {
    let mut m = LadderMetrics::default();
    for row in rows {
        let LadderRow::Decision {
            tool_call_id,
            keep_result,
            ..
        } = row
        else {
            continue;
        };
        let reread = rows
            .iter()
            .any(|row| matches!(row, LadderRow::Reread { matched_tool_call_id, .. } if matched_tool_call_id == tool_call_id));
        let predicted = keep_result.is_some_and(|score| score >= threshold);
        match (predicted, reread) {
            (true, true) => m.tp += 1,
            (true, false) => m.fp += 1,
            (false, true) => m.fn_count += 1,
            (false, false) => m.tn += 1,
        }
    }
    m.precision = (m.tp + m.fp > 0).then(|| m.tp as f64 / (m.tp + m.fp) as f64);
    m.recall = (m.tp + m.fn_count > 0).then(|| m.tp as f64 / (m.tp + m.fn_count) as f64);
    m
}

/// The match key for "the agent re-issued this call": the tool name plus a canonical spelling of
/// the input. `serde_json`'s `Value` prints objects with sorted keys, so two calls with the same
/// arguments sign identically however the runner ordered the fields — and both sides arrive as
/// `Value`s parsed by the same deserialiser, so no second canonicalisation is needed.
pub(crate) fn signature(name: &str, input: &Value) -> String {
    format!("{name} {input}")
}

/// Per-session watch state (`Runtime.jev_ladder`): what each recent tool call looked like, and
/// which pending decisions still await their reread. In memory only, like `judged_questions`: a
/// mothership restart forgets the watchlist, so rereads that would have landed after it are simply
/// not counted — a lost measurement, never a wrong one.
#[derive(Default)]
pub(crate) struct Watch {
    /// `tool_call_id -> (tool, input signature)` for the most recent [`CALL_MEMORY`] calls.
    calls: HashMap<String, (String, String)>,
    /// Insertion order into `calls`, so the oldest entry is the one evicted.
    order: VecDeque<String>,
    /// Pending decisions, oldest first: `(tool_call_id of the original, tool, signature)`.
    pending: Vec<(String, String, String)>,
}

impl Watch {
    /// Records a tool call as a candidate original a later decision may name.
    fn note_call(&mut self, tool_call_id: &str, tool: &str, sig: &str) {
        if self.calls.remove(tool_call_id).is_none()
            && self.calls.len() >= CALL_MEMORY
            && let Some(oldest) = self.order.pop_front()
        {
            self.calls.remove(&oldest);
        }
        self.calls
            .insert(tool_call_id.to_string(), (tool.to_string(), sig.to_string()));
        self.order.push_back(tool_call_id.to_string());
    }

    /// Arms the watch: from now, the first new call with this original's tool and input counts as
    /// its decision's reread, once.
    fn watch_decision(&mut self, tool_call_id: &str, tool: &str, sig: &str) {
        self.pending
            .push((tool_call_id.to_string(), tool.to_string(), sig.to_string()));
    }

    /// The original whose pending decision this new call re-issued, if any. The entry is spent on
    /// the match, so repeating a call many times cannot inflate recall; the oldest match wins, so
    /// equal-argument decisions are confirmed in the order they were decided.
    fn take_reread(&mut self, tool: &str, sig: &str) -> Option<String> {
        let at = self.pending.iter().position(|(_, t, s)| t == tool && s == sig)?;
        let (id, _, _) = self.pending.remove(at);
        Some(id)
    }

    /// What the original call a decision names looked like, if it is still in memory.
    fn original(&self, tool_call_id: &str) -> Option<(String, String)> {
        self.calls.get(tool_call_id).cloned()
    }
}

/// The `jev_ladder` dispatch arm (events.rs keeps it thin, like `loops.rs`): an applied pass logs
/// one decision row per chunk and arms the reread watch for each chunk whose original call it can
/// still name; a fallback pass (`applied: false`) changed nothing, so it leaves nothing behind.
pub(crate) async fn on_ladder(app: &Shared, id: &str, rt: &Arc<Runtime>, applied: bool, decisions: &[JevDecision]) {
    if !applied {
        return;
    }
    let mut watch = rt.jev_ladder.lock().await;
    for d in decisions {
        let row = LadderRow::Decision {
            ts: Utc::now(),
            session: id.to_string(),
            tool_call_id: d.tool_call_id.clone(),
            tool: d.tool.clone(),
            action: d.action.as_str().to_string(),
            keep_call: d.keep_call,
            keep_result: d.keep_result,
        };
        if let Some(line) = row.line()
            && let Err(e) = append_line(&app.jev_ladder_file(), &line).await
        {
            // A lost row is a lost measurement, not a failed colony: the same deal the routing
            // ledger's appends get.
            app.storage_failed("append to the jev ladder ledger", &e).await;
        }
        // The decision needs its original call's input to recognise a re-issue later; a decision
        // whose original has fallen out of memory (or never arrived on this stream) still has its
        // row, it just cannot be graded.
        if let Some((tool, sig)) = watch.original(&d.tool_call_id) {
            watch.watch_decision(&d.tool_call_id, &tool, &sig);
        }
    }
}

/// The `tool_call` half, read off the raw line by the dispatcher (the type is forwarded-only and
/// never reaches the `AgentEvent` match, so this is where its harness use lives). A new call is
/// first a candidate reread — a re-issue of something a pending decision watches — and then the
/// next original a later decision may name.
pub(crate) async fn note_tool_call(app: &Shared, id: &str, rt: &Arc<Runtime>, event: &Value) {
    let (Some(tool_call_id), Some(tool)) = (event["tool_call_id"].as_str(), event["name"].as_str()) else {
        return;
    };
    let sig = signature(tool, event.get("input").unwrap_or(&Value::Null));
    let matched = {
        let mut watch = rt.jev_ladder.lock().await;
        // A runner reusing the original's own id is not a re-issue of it.
        let matched = watch.take_reread(tool, &sig).filter(|original| original != tool_call_id);
        watch.note_call(tool_call_id, tool, &sig);
        matched
    };
    let Some(original) = matched else { return };
    let row = LadderRow::Reread {
        ts: Utc::now(),
        session: id.to_string(),
        tool_call_id: tool_call_id.to_string(),
        matched_tool_call_id: original.clone(),
        tool: tool.to_string(),
    };
    let Some(line) = row.line() else { return };
    if let Err(e) = append_line(&app.jev_ladder_file(), &line).await {
        app.storage_failed("append to the jev ladder ledger", &e).await;
        return;
    }
    report_metrics(app, id, &original).await;
}

/// The measurement's one visible heartbeat, in the colony's harness log when a reread lands: the
/// running precision/recall over every decision row in the ledger, all colonies together — that is
/// what stage 2's report will be over, and a person watching a colony should not have to wait for
/// it to see the ladder working. The whole-ledger read happens per reread, which is rare by
/// construction (a decision is confirmed at most once).
async fn report_metrics(app: &Shared, id: &str, original: &str) {
    let rows = read_rows(&app.jev_ladder_file());
    let m = precision_recall(&rows, PREDICTED_THRESHOLD);
    let score = |v: Option<f64>| v.map(|v| format!("{v:.2}")).unwrap_or_else(|| "n/a".into());
    app.session_log(
        id,
        "info",
        format!(
            "jev_ladder: a re-issued call confirmed the decision on {original}; running score over {} \
             decision row(s): precision {}, recall {} (tp {}, fp {}, fn {}, tn {})",
            m.tp + m.fp + m.fn_count + m.tn,
            score(m.precision),
            score(m.recall),
            m.tp,
            m.fp,
            m.fn_count,
            m.tn
        ),
    )
    .await;
}

/// Reads the ledger back. A file that is not there yet, a torn line, a foreign line: each costs
/// its row, not the report — the ledger is measurement.
fn read_rows(path: &Path) -> Vec<LadderRow> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    std::str::from_utf8(&bytes)
        .map(|text| text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::handle_agent_event;
    use crate::sessions::{SessionStatus, tests::app_with_colony};

    fn decision(id: &str, keep_result: f64) -> LadderRow {
        LadderRow::Decision {
            ts: Utc::now(),
            session: "abc".into(),
            tool_call_id: id.into(),
            tool: "Bash".into(),
            action: "drop_result".into(),
            keep_call: Some(0.5),
            keep_result: Some(keep_result),
        }
    }

    fn reread(new_id: &str, matched: &str) -> LadderRow {
        LadderRow::Reread {
            ts: Utc::now(),
            session: "abc".into(),
            tool_call_id: new_id.into(),
            matched_tool_call_id: matched.into(),
            tool: "Bash".into(),
        }
    }

    #[test]
    fn every_prediction_confirmed_by_a_reread_is_a_true_positive() {
        let rows = vec![decision("a", 0.9), decision("b", 0.8), reread("x", "a"), reread("y", "b")];
        let m = precision_recall(&rows, PREDICTED_THRESHOLD);
        assert_eq!((m.tp, m.fp, m.fn_count, m.tn), (2, 0, 0, 0));
        assert_eq!(m.precision, Some(1.0));
        assert_eq!(m.recall, Some(1.0));
    }

    #[test]
    fn the_four_outcomes_are_counted_separately() {
        let rows = vec![
            decision("tp", 0.9),
            decision("fp", 0.8),
            decision("fn", 0.2),
            decision("tn", 0.1),
            reread("x", "tp"),
            reread("y", "fn"),
        ];
        let m = precision_recall(&rows, PREDICTED_THRESHOLD);
        assert_eq!((m.tp, m.fp, m.fn_count, m.tn), (1, 1, 1, 1));
        assert_eq!(m.precision, Some(0.5));
        assert_eq!(m.recall, Some(0.5));
    }

    #[test]
    fn an_empty_ledger_measures_nothing_without_panicking() {
        let m = precision_recall(&[], PREDICTED_THRESHOLD);
        assert_eq!(m, LadderMetrics::default());
        assert_eq!(m.precision, None);
        assert_eq!(m.recall, None);
    }

    /// The boundary is deliberately `>=`: "what scores high enough to keep" keeps a chunk at the
    /// threshold, so a score exactly there predicted positively.
    #[test]
    fn a_score_exactly_at_the_threshold_is_predicted_positive() {
        let m = precision_recall(&[decision("a", PREDICTED_THRESHOLD), reread("x", "a")], PREDICTED_THRESHOLD);
        assert_eq!((m.tp, m.fp, m.fn_count, m.tn), (1, 0, 0, 0));
        let m = precision_recall(&[decision("a", 0.49), reread("x", "a")], PREDICTED_THRESHOLD);
        assert_eq!((m.tp, m.fp, m.fn_count, m.tn), (0, 0, 1, 0), "just below is not");
    }

    #[test]
    fn with_no_positive_predictions_precision_is_none_not_a_zero_score() {
        let rows = vec![decision("a", 0.1), decision("b", 0.0), reread("x", "a")];
        let m = precision_recall(&rows, PREDICTED_THRESHOLD);
        assert_eq!(
            m.precision, None,
            "no predictions: a denominator of zero, not a score of zero"
        );
        assert_eq!(m.recall, Some(0.0), "a confirmed need the plugin failed to predict");
    }

    /// An unscored chunk predicts nothing either way: it is neither a false positive nor a true
    /// negative that flatters precision.
    #[test]
    fn an_unscored_decision_is_never_predicted_positive() {
        let row = LadderRow::Decision {
            ts: Utc::now(),
            session: "abc".into(),
            tool_call_id: "a".into(),
            tool: "Bash".into(),
            action: "keep".into(),
            keep_call: None,
            keep_result: None,
        };
        let m = precision_recall(&[row, reread("x", "a")], PREDICTED_THRESHOLD);
        assert_eq!((m.tp, m.fp, m.fn_count, m.tn), (0, 0, 1, 0));
    }

    #[test]
    fn rows_round_trip_through_their_jsonl_line_with_the_kind_tag() {
        let line = decision("a", 0.87).line().expect("a row of plain data serialises");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["kind"], "decision");
        assert_eq!(v["tool_call_id"], "a");
        assert!(matches!(
            serde_json::from_str::<LadderRow>(&line).unwrap(),
            LadderRow::Decision { tool_call_id, keep_result: Some(0.87), .. } if tool_call_id == "a"
        ));
        let line = reread("x", "a").line().expect("a row of plain data serialises");
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["kind"], "reread");
        assert_eq!(v["matched_tool_call_id"], "a");
    }

    #[test]
    fn the_signature_is_stable_across_key_order_and_sensitive_to_tool_and_input() {
        let a = signature(
            "Bash",
            &serde_json::from_str::<Value>(r#"{"command":"ls","cwd":"/w"}"#).unwrap(),
        );
        let b = signature(
            "Bash",
            &serde_json::from_str::<Value>(r#"{"cwd":"/w","command":"ls"}"#).unwrap(),
        );
        assert_eq!(a, b, "the canonical spelling must not depend on the runner's field order");
        assert_ne!(
            a,
            signature("Bash", &serde_json::from_str::<Value>(r#"{"command":"ls -la"}"#).unwrap())
        );
        assert_ne!(
            a,
            signature(
                "Read",
                &serde_json::from_str::<Value>(r#"{"command":"ls","cwd":"/w"}"#).unwrap()
            )
        );
        // A call without input signs differently from one with an empty object, and from itself
        // with any input: missing and empty are not the same arguments.
        assert_ne!(a, signature("Bash", &Value::Null));
        assert_ne!(signature("Bash", &Value::Null), signature("Bash", &serde_json::json!({})));
    }

    #[test]
    fn the_watch_spends_each_decision_on_the_first_matching_call_only() {
        let mut w = Watch::default();
        w.note_call("t1", "Bash", "ls");
        w.watch_decision("t1", "Bash", "ls");
        w.watch_decision("t2", "Bash", "ls");
        assert_eq!(w.take_reread("Bash", "ls").as_deref(), Some("t1"), "oldest decision first");
        assert_eq!(w.take_reread("Bash", "ls").as_deref(), Some("t2"));
        assert_eq!(w.take_reread("Bash", "ls"), None, "each decision counts once");
        assert_eq!(w.take_reread("Read", "ls"), None, "the tool name is part of the match");
    }

    #[test]
    fn the_call_memory_evicts_its_oldest_entry_past_the_cap() {
        let mut w = Watch::default();
        for i in 0..CALL_MEMORY {
            w.note_call(&format!("t{i}"), "Bash", &format!("c{i}"));
        }
        assert_eq!(w.original("t0").map(|(_, sig)| sig), Some("c0".into()));
        w.note_call("new", "Bash", "n");
        assert_eq!(w.original("t0"), None, "the oldest call is the one evicted");
        assert_eq!(w.original("t1").map(|(_, sig)| sig), Some("c1".into()));
        assert_eq!(w.original("new").map(|(_, sig)| sig), Some("n".into()));
    }

    /// The whole stage-1 path through the live dispatcher: a call arms nothing on its own, an
    /// applied pass logs its decisions (an unresolvable one included), a matching later call logs
    /// exactly one reread, the spent decision matches no more, and a fallback pass
    /// (`applied: false`) leaves nothing behind.
    #[tokio::test]
    async fn the_ladder_measures_decisions_and_their_rereads_end_to_end() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let rt = app.runtime("abc").await;
        let call = |seq: u64, id: &str| {
            format!(
                r#"{{"seq":{seq},"type":"tool_call","message_id":"m","tool_call_id":"{id}","name":"Bash","input":{{"command":"ls -la"}}}}"#
            )
        };
        handle_agent_event(&app, "abc", &rt, &call(1, "toolu_a")).await;
        let ladder = r#"{"seq":2,"type":"jev_ladder","applied":true,"pre_tokens":12000,"post_tokens":8000,"trigger":"auto","decisions":[
            {"tool_call_id":"toolu_a","tool":"Bash","action":"drop_result","keep_call":0.12,"keep_result":0.05},
            {"tool_call_id":"toolu_gone","tool":"Bash","action":"keep","keep_call":0.98,"keep_result":0.9}]}"#;
        handle_agent_event(&app, "abc", &rt, ladder).await;
        let ledger = read_rows(&app.jev_ladder_file());
        assert_eq!(
            ledger.len(),
            2,
            "both decisions are logged, the unresolvable one included: {ledger:?}"
        );

        handle_agent_event(&app, "abc", &rt, &call(3, "toolu_b")).await;
        let ledger = read_rows(&app.jev_ladder_file());
        assert_eq!(ledger.len(), 3, "{ledger:?}");
        assert!(matches!(
            &ledger[2],
            LadderRow::Reread { tool_call_id, matched_tool_call_id, .. }
                if tool_call_id == "toolu_b" && matched_tool_call_id == "toolu_a"
        ));

        // The decision is spent: the same call again is not a second reread ...
        handle_agent_event(&app, "abc", &rt, &call(4, "toolu_c")).await;
        assert_eq!(read_rows(&app.jev_ladder_file()).len(), 3);
        // ... and a pass that was computed but not applied leaves nothing behind.
        let fallback = r#"{"seq":5,"type":"jev_ladder","applied":false,"pre_tokens":8000,"post_tokens":8000,"trigger":"auto","decisions":[
            {"tool_call_id":"toolu_b","tool":"Bash","action":"keep","keep_call":0.9,"keep_result":0.95}]}"#;
        handle_agent_event(&app, "abc", &rt, fallback).await;
        assert_eq!(read_rows(&app.jev_ladder_file()).len(), 3);

        // The heartbeat named the confirmed original.
        let logs = rt.logs.lock().await;
        assert!(logs.iter().any(|l| {
            l["message"]
                .as_str()
                .is_some_and(|m| m.contains("jev_ladder") && m.contains("toolu_a"))
        }));
        drop(logs);
        let _ = std::fs::remove_dir_all(root);
    }
}
