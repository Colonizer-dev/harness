//! Paged reads of a colony's event log, and the summary the cockpit shows instead of replaying all
//! of it (issue #1210).
//!
//! A long-running colony has thousands of events across `events.jsonl` (this run) and the rotated
//! `events-N.jsonl` (earlier runs). Opening it used to send every line of `events.jsonl` and make
//! the browser render them all before anything showed. [`page`] reads a window from the *end* of a
//! segment, steps back through the rotated ones, and never touches the part of the file older than
//! the page it was asked for, so its cost does not grow with the history. [`Summary`] carries what
//! the UI derives from the whole history (settlers, cost, the brief) so it need not replay it.

use super::*;
use crate::store::{SessionStore, event_archives};
use std::collections::HashMap;

/// Events in a page when the caller names no limit.
pub(crate) const DEFAULT_LIMIT: usize = 200;
/// The most events one page carries, whatever the caller asks for.
pub(crate) const MAX_LIMIT: usize = 1000;
/// How much of a segment one read takes; doubled when a single line is bigger than that.
const CHUNK: u64 = 256 * 1024;
/// How far back from a page the previous `turn_end` is searched for its usage baseline.
const BASELINE_SCAN: u64 = 4 * 1024 * 1024;

/// Where an event sits in the colony's logs: the run (`epoch`, the archive number or the current
/// run's), its `seq` (per-run) and the byte `offset` its line starts at in that run's segment. A
/// page's oldest event is the cursor the next, older page is asked for with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Cursor {
    pub epoch: u64,
    pub seq: u64,
    pub offset: u64,
}

/// What an older page is asked for: events of run `epoch` with `seq` below `seq`. `offset`, the
/// byte the cursor's line starts at, lets the read seek straight there; without it (or when it is
/// stale) the segment is read from its end and filtered by seq.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Before {
    pub epoch: u64,
    pub seq: u64,
    pub offset: Option<u64>,
}

/// One page of events, oldest first.
#[derive(Debug)]
pub(crate) struct Page {
    pub events: Vec<Value>,
    /// Whether anything older is still on record.
    pub has_more: bool,
    /// The oldest event on the page — the cursor for the next one. A run with no events at all
    /// names the start of its segment, so the next page continues into the rotated logs.
    pub oldest: Cursor,
    /// The colony-cumulative `model_usage` as of the last `turn_end` before this page, which the
    /// UI diffs the page's first `turn_end` against to name the models that served the turn.
    pub baseline_usage: Option<Value>,
}

impl Page {
    /// The wire fields every page carries, for the events socket's `history` frame and the HTTP
    /// reply alike.
    pub(crate) fn meta(&self) -> Value {
        json!({
            "has_more": self.has_more,
            "oldest_seq": self.oldest.seq,
            "epoch": self.oldest.epoch,
            "offset": self.oldest.offset,
            "baseline_usage": self.baseline_usage,
        })
    }
}

fn segment(epoch: u64, current: u64) -> String {
    if epoch == current {
        "events.jsonl".into()
    } else {
        format!("events-{epoch}.jsonl")
    }
}

/// A line that starts a turn: the page ends on one so a turn is never split from its first message.
fn starts_turn(event: &Value) -> bool {
    event["type"] == "user_message"
}

/// Reads one page of at least `limit` events (the most recent ones below `before`, or of the whole
/// log when `before` is `None`), extended back to the start of the turn it would otherwise cut, up
/// to four times `limit`. With `across_runs` false only the segment `before` names is read (the
/// socket's first paint, which shows this run); with it true the page continues into the rotated
/// logs, newest first, when a segment runs out.
pub(crate) async fn page(
    store: &dyn SessionStore,
    id: &str,
    current: u64,
    before: Option<Before>,
    limit: usize,
    across_runs: bool,
) -> std::io::Result<Page> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let cap = limit * 4;
    let archives = event_archives(store, id).await?;
    let mut epoch = before.map_or(current, |b| b.epoch);
    let mut end = before.and_then(|b| b.offset);
    let mut below = before.map(|b| b.seq);
    let mut hinted = end.is_some();
    // Newest first while collecting.
    let mut got: Vec<(Cursor, Value)> = Vec::new();
    let mut stopped = false;
    'segments: loop {
        let name = segment(epoch, current);
        let mut chunk = CHUNK;
        loop {
            let Some((bytes, start)) = store.read_before(id, &name, end, chunk).await? else {
                break;
            };
            if hinted {
                hinted = false;
                // A seek hint lands on a line start, so the window ends in a newline; anything else
                // is a stale offset, and the segment is read from its end instead.
                if end.is_some_and(|e| e > 0) && !bytes.is_empty() && bytes.last() != Some(&b'\n') {
                    end = None;
                    continue;
                }
            }
            if bytes.is_empty() && start > 0 {
                // One line is bigger than the window.
                chunk = chunk.saturating_mul(2);
                continue;
            }
            let mut spans = Vec::new();
            let mut at = 0usize;
            for line in bytes.split(|b| *b == b'\n') {
                if !line.is_empty() {
                    spans.push((start + at as u64, line));
                }
                at += line.len() + 1;
            }
            for (offset, line) in spans.into_iter().rev() {
                let Ok(mut event) = serde_json::from_slice::<Value>(line) else {
                    continue;
                };
                let Some(seq) = event["seq"].as_u64() else { continue };
                if below.is_some_and(|b| seq >= b) {
                    continue;
                }
                if epoch != current {
                    // A rotated log written before redaction existed can still carry a secret (#761).
                    crate::redact::redact_value(&mut event);
                }
                let turn_start = starts_turn(&event);
                got.push((Cursor { epoch, seq, offset }, event));
                if got.len() >= limit && (turn_start || got.len() >= cap) {
                    stopped = true;
                    break 'segments;
                }
            }
            chunk = CHUNK;
            if start == 0 {
                break;
            }
            end = Some(start);
        }
        // This segment is read to its start. The next older run, when there is one and the read may
        // cross into it.
        let Some(previous) = archives.iter().rev().find(|&&n| n < epoch).copied() else {
            break;
        };
        if !across_runs {
            stopped = true;
            break;
        }
        epoch = previous;
        end = None;
        below = None;
    }
    let oldest = got.last().map(|(c, _)| *c).unwrap_or(Cursor {
        epoch,
        seq: 1,
        offset: 0,
    });
    let has_more = if stopped && got.last().is_some_and(|(c, _)| c.offset > 0) {
        true
    } else {
        archives.iter().any(|&n| n < oldest.epoch)
    };
    let baseline_usage = if got.is_empty() {
        None
    } else {
        baseline(store, id, &segment(oldest.epoch, current), oldest.offset).await
    };
    got.reverse();
    Ok(Page {
        events: got.into_iter().map(|(_, e)| e).collect(),
        has_more,
        oldest,
        baseline_usage,
    })
}

/// The `model_usage` of the last `turn_end` before byte `offset` of one segment, looking back at
/// most [`BASELINE_SCAN`] bytes. `None` at the start of a run (a resumed colony's runner starts its
/// totals over) and when no turn ended in range.
async fn baseline(store: &dyn SessionStore, id: &str, name: &str, offset: u64) -> Option<Value> {
    let mut end = offset;
    let mut scanned = 0u64;
    let mut chunk = CHUNK;
    while end > 0 && scanned < BASELINE_SCAN {
        let (bytes, start) = store.read_before(id, name, Some(end), chunk).await.ok()??;
        if bytes.is_empty() && start > 0 {
            chunk = chunk.saturating_mul(2);
            continue;
        }
        for line in bytes.split(|b| *b == b'\n').rev() {
            // Cheap test first: most lines are not a turn end.
            if line.is_empty() || !line.windows(10).any(|w| w == b"\"turn_end\"") {
                continue;
            }
            let Ok(event) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            if event["type"] == "turn_end"
                && let Some(usage) = event
                    .get("model_usage")
                    .filter(|u| u.as_object().is_some_and(|o| !o.is_empty()))
            {
                return Some(usage.clone());
            }
        }
        scanned += end - start;
        chunk = CHUNK;
        end = start;
    }
    None
}

/// One settler (subagent) as the summary knows it, in order of first appearance.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Settler {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub steps: u64,
    pub errors: u64,
    pub last_tool: Option<String>,
}

/// What the cockpit derives from the *whole* of this run's events, kept so it need not load them:
/// the settlers, the cost, the turn count, the brief and the colony's last reported state. Built from
/// the log once per run and then kept current by every event that is broadcast.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct Summary {
    pub last_seq: u64,
    pub events: u64,
    pub turns: u64,
    pub cost_usd: Option<f64>,
    /// The colony's first message (`user_message` with id `initial`), as the line was recorded.
    pub brief: Option<Value>,
    pub model: Option<String>,
    pub agent_state: Option<Value>,
    pub settlers: Vec<Settler>,
    #[serde(skip)]
    index: HashMap<String, usize>,
    #[serde(skip)]
    calls: HashMap<String, usize>,
}

impl Summary {
    pub(crate) fn from_log(bytes: &[u8]) -> Self {
        let mut summary = Self::default();
        for line in bytes.split(|b| *b == b'\n') {
            if let Ok(event) = serde_json::from_slice::<Value>(line) {
                summary.apply(&event);
            }
        }
        summary
    }

    /// Folds one event in. Events at or below the last seq seen are ignored, so feeding the log and
    /// the live broadcasts that overlap it twice is harmless.
    pub(crate) fn apply(&mut self, event: &Value) {
        let Some(seq) = event["seq"].as_u64() else { return };
        if seq <= self.last_seq {
            return;
        }
        self.last_seq = seq;
        self.events += 1;
        let settler = event.get("agent").and_then(|a| {
            let id = a.get("id")?.as_str()?;
            Some(*self.index.entry(id.to_string()).or_insert_with(|| {
                self.settlers.push(Settler {
                    id: id.to_string(),
                    name: a.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    description: a.get("description").and_then(Value::as_str).map(str::to_string),
                    steps: 0,
                    errors: 0,
                    last_tool: None,
                });
                self.settlers.len() - 1
            }))
        });
        match event["type"].as_str() {
            Some("user_message") if self.brief.is_none() && event["id"] == "initial" => self.brief = Some(event.clone()),
            Some("status") => self.agent_state = Some(json!({"state": event["state"], "detail": event["detail"]})),
            Some("model_changed") => self.model = event["model"].as_str().map(str::to_string),
            Some("tool_call") => {
                if let (Some(i), Some(call)) = (settler, event["tool_call_id"].as_str()) {
                    self.settlers[i].steps += 1;
                    self.settlers[i].last_tool = event["name"].as_str().map(str::to_string);
                    self.calls.insert(call.to_string(), i);
                }
            }
            Some("tool_result") => {
                if let Some(i) = event["tool_call_id"].as_str().and_then(|c| self.calls.remove(c))
                    && event["is_error"] == true
                {
                    self.settlers[i].errors += 1;
                }
            }
            Some("turn_end") => {
                self.turns += 1;
                if let Some(cost) = event["cost_usd"].as_f64() {
                    self.cost_usd = Some(cost);
                }
            }
            _ => {}
        }
    }
}

/// Where a run's [`Summary`] stands. Cold until a socket first asks; Building while the log is
/// being read (broadcasts in that window wait in the buffer); Ready after, kept current by every
/// broadcast.
#[derive(Debug, Default)]
pub(crate) enum SummaryState {
    #[default]
    Cold,
    Building(Vec<Value>),
    Ready(Box<Summary>),
}

impl Runtime {
    /// Called with every broadcast event line; folds it into the summary once one exists.
    pub(crate) fn note_summary(&self, seq: Option<u64>, json: &str) {
        if seq.is_none() {
            return;
        }
        let mut state = self.summary.lock().unwrap_or_else(|p| p.into_inner());
        if matches!(*state, SummaryState::Cold) {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        match &mut *state {
            SummaryState::Building(buffer) => buffer.push(event),
            SummaryState::Ready(summary) => summary.apply(&event),
            SummaryState::Cold => {}
        }
    }

    /// The run's summary: the kept one, or one built from `events.jsonl` now (and kept, unless
    /// another caller is already building it).
    pub(crate) async fn summary(&self, store: &dyn SessionStore, id: &str) -> Summary {
        let claimed = {
            let mut state = self.summary.lock().unwrap_or_else(|p| p.into_inner());
            match &*state {
                SummaryState::Ready(summary) => return (**summary).clone(),
                SummaryState::Cold => {
                    *state = SummaryState::Building(Vec::new());
                    true
                }
                SummaryState::Building(_) => false,
            }
        };
        let bytes = store.read_file(id, "events.jsonl").await.ok().flatten().unwrap_or_default();
        let mut summary = Summary::from_log(&bytes);
        if !claimed {
            return summary;
        }
        let mut state = self.summary.lock().unwrap_or_else(|p| p.into_inner());
        if let SummaryState::Building(buffer) = std::mem::take(&mut *state) {
            for event in &buffer {
                summary.apply(event);
            }
        }
        *state = SummaryState::Ready(Box::new(summary.clone()));
        summary
    }
}

#[cfg(test)]
mod tests;
