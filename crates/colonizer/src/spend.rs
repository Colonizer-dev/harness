//! Per-org spend: the live rollup behind `GET /api/orgs`, and the append-only daily journal behind
//! `GET /api/spend/history`. The two share one aggregation shape, so a dollar and a token mean the
//! same thing on both surfaces.
//!
//! The journal is a `spend.jsonl` in the data dir, one JSON line per event, written by the same
//! `util::append_line` the routing ledger uses. Append-only is the point: a colony's cleanup or
//! deletion must not lose the spend it left behind, so the file lives next to `sessions.json` and
//! is never rewritten or touched by the handlers that forget a colony. Every row carries the UTC
//! day it belongs to, so a day's rollup never changes after the fact.

use crate::{App, Shared, sessions::Session, util::append_line};
use axum::{
    Json,
    extract::{Query, State},
};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::BufRead,
    path::{Path, PathBuf},
};

/// The four token counts a model reports at turn end, in Anthropic's terms.
#[derive(Clone, Copy, Debug, Default)]
struct Tokens {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Tokens {
    /// Saturated so a journal row with absurd counts can never wrap a total around to a small
    /// number; the four fields add the same way the rolling accumulation does.
    fn total(self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// The increment one turn added over the last cumulative, floored at zero so a cheaper
    /// re-estimate never writes a negative line.
    fn saturating_sub(self, before: Tokens) -> Tokens {
        Tokens {
            input: self.input.saturating_sub(before.input),
            output: self.output.saturating_sub(before.output),
            cache_read: self.cache_read.saturating_sub(before.cache_read),
            cache_write: self.cache_write.saturating_sub(before.cache_write),
        }
    }
}

/// One model's token and (attributed) cost totals inside an org's spend.
#[derive(Clone, Debug, Default)]
pub(crate) struct ModelSpend {
    pub(crate) tokens: u64,
    pub(crate) cost_usd: Option<f64>,
}

/// Spend summed for one org over one window — the live sessions of `GET /api/orgs`, or one day of
/// the journal behind `GET /api/spend/history`. Same accumulation for both, so the two can't
/// disagree on what a dollar or a token is. `cost_usd` and `routed_cost_usd` stay `None` until a
/// measurement first reports them, never `Some(0.0)`: an unmeasured dollar is not the same as a
/// zero one, and the same goes for a model's cost.
#[derive(Clone, Debug, Default)]
pub(crate) struct OrgSpend {
    pub(crate) cost_usd: Option<f64>,
    pub(crate) routed_cost_usd: Option<f64>,
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_write_tokens: u64,
    /// Per-model totals, keyed by name. A model appears once it has anything to report; its cost
    /// stays `None` until some session attributed one to it.
    pub(crate) models: BTreeMap<String, ModelSpend>,
}

impl OrgSpend {
    /// One dollar measured on a session's own (`cost_usd`) channel.
    fn add_claude_cost(&mut self, cost: f64) {
        self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
    }

    /// One dollar the gateway routed and priced, on the separate `routed_cost_usd` channel.
    fn add_routed_cost(&mut self, cost: f64) {
        self.routed_cost_usd = Some(self.routed_cost_usd.unwrap_or(0.0) + cost);
    }

    /// One row's dollar figure, when the row carries one and on which channel the row's kind says.
    fn add_cost(&mut self, cost: Option<f64>) {
        if let Some(cost) = cost {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
        }
    }

    fn add_routed_from(&mut self, cost: Option<f64>) {
        if let Some(cost) = cost {
            self.routed_cost_usd = Some(self.routed_cost_usd.unwrap_or(0.0) + cost);
        }
    }

    fn add_tokens(&mut self, tokens: Tokens) {
        self.input_tokens = self.input_tokens.saturating_add(tokens.input);
        self.output_tokens = self.output_tokens.saturating_add(tokens.output);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(tokens.cache_read);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(tokens.cache_write);
    }

    /// One model's tokens, and its attributed cost when the row carries one.
    fn add_model(&mut self, model: &str, tokens: Tokens, cost: Option<f64>) {
        let model = self.models.entry(model.to_string()).or_default();
        model.tokens = model.tokens.saturating_add(tokens.total());
        if let Some(cost) = cost {
            model.cost_usd = Some(model.cost_usd.unwrap_or(0.0) + cost);
        }
    }

    /// Gives a session's Claude-reported cost (`cost_usd`) to its one model: the
    /// cost-attribution rule, applied once per session by [`rollup_sessions`] and per turn by the
    /// journal. Routed dollars never ride on a model row — the gateway prices whole responses and
    /// cannot say which of its models served one — so they reach the org totals' `routed_cost_usd`
    /// and nowhere else.
    fn attribute_cost(&mut self, model: &str, cost: f64) {
        let model = self.models.entry(model.to_string()).or_default();
        model.cost_usd = Some(model.cost_usd.unwrap_or(0.0) + cost);
    }
}

/// The token counts per model in a `model_usage` document — `{model: {input_tokens, output_tokens,
/// cache_read_tokens, cache_write_tokens}}`, each key optional. Anything else (a non-object, a
/// malformed model entry, a missing count) reads as no tokens for that part, never as an error: a
/// runner that has not started reporting a key yet means zero for it.
fn model_tokens(usage: Option<&Value>) -> Vec<(&str, Tokens)> {
    let Some(Value::Object(models)) = usage else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|(model, counts)| {
            let counts = counts.as_object()?;
            if model.is_empty() {
                return None;
            }
            let token = |key: &str| counts.get(key).and_then(Value::as_u64).unwrap_or(0);
            Some((
                model.as_str(),
                Tokens {
                    input: token("input_tokens"),
                    output: token("output_tokens"),
                    cache_read: token("cache_read_tokens"),
                    cache_write: token("cache_write_tokens"),
                },
            ))
        })
        .collect()
}

/// Sums one org's sessions into the spend shape, applying the cost-attribution rule: a session's
/// Claude-reported cost (`cost_usd`) belongs to its model only when the session used exactly one
/// model. A multi-model session's cost lands in the org totals but in no model's row; the pretend
/// per-model split a dollar would need is never fabricated, and a model's cost reads `null` until
/// some session attributed one. Gateway-routed dollars never ride on a model's row — the journal
/// has no model on its `routed` rows, and neither does this rollup — so a routed dollar is an
/// org-total figure on both surfaces, and the two can't disagree per model. Each turn's journal
/// rows follow the same rule.
pub(crate) fn rollup_sessions<'a>(sessions: impl IntoIterator<Item = &'a Session>) -> OrgSpend {
    let mut spend = OrgSpend::default();
    for s in sessions {
        if let Some(cost) = s.cost_usd {
            spend.add_claude_cost(cost);
        }
        if let Some(cost) = s.routed_cost_usd {
            spend.add_routed_cost(cost);
        }
        let models = model_tokens(s.model_usage.as_ref());
        for (model, tokens) in &models {
            if tokens.total() > 0 {
                spend.add_tokens(*tokens);
                spend.add_model(model, *tokens, None);
            }
        }
        if models.len() == 1
            && let Some(cost) = s.cost_usd
        {
            spend.attribute_cost(models[0].0, cost);
        }
    }
    spend
}

/// The `spend` object both endpoints embed: `cost_usd` and `routed_cost_usd` null until something
/// measured them, `tokens` as zeroes until a turn reported them, and `models` largest first (ties
/// by name) for a deterministic UI. A model an org's sessions named but nothing priced still
/// appears, with a null cost.
pub(crate) fn spend_json(spend: &OrgSpend) -> Value {
    let mut models: Vec<(&String, &ModelSpend)> = spend
        .models
        .iter()
        .filter(|(_, m)| m.tokens > 0 || m.cost_usd.is_some())
        .collect();
    models.sort_by(|(a, left), (b, right)| right.tokens.cmp(&left.tokens).then_with(|| a.cmp(b)));
    json!({
        "cost_usd": spend.cost_usd,
        "routed_cost_usd": spend.routed_cost_usd,
        "tokens": {
            "input": spend.input_tokens,
            "output": spend.output_tokens,
            "cache_read": spend.cache_read_tokens,
            "cache_write": spend.cache_write_tokens,
        },
        "models": models
            .into_iter()
            .map(|(model, m)| json!({"model": model, "tokens": m.tokens, "cost_usd": m.cost_usd}))
            .collect::<Vec<Value>>(),
    })
}

// ---------------------------------------------------------------------------
// The journal (docs/protocol.md §6.7): `spend.jsonl` in the data dir, append-only.
// ---------------------------------------------------------------------------

/// One journal row: an event's spend shape, as written and read back. `kind` says what the event
/// was — `usage` (a turn's model tokens and cost), `routed` (a gateway response's priced cost),
/// `launched` or `returned` (a colony's run edges). Token fields default to 0 and `model` and
/// `cost_usd` are omitted while unknown, so a row from a build that disagrees about a field still
/// parses (`#[serde(default)]`), and a line that is not a row at all is skipped by the reader.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SpendRow {
    ts: String,
    day: String,
    org: String,
    kind: String,
    /// The colony the row belongs to (`Session::id`) and the agent module — the harness — that ran
    /// it (issue #296), so spend can later be grouped per harness as well as per org. Colony-scoped
    /// rows carry both; chat rows carry neither, and rows a build before the fields existed wrote
    /// neither. Omitted while absent, never an empty string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
}

fn spend_file(data_dir: &Path) -> PathBuf {
    data_dir.join("spend.jsonl")
}

/// Today's UTC date in the journal's `YYYY-MM-DD` spelling, the day every row is filed under.
fn today() -> String {
    Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

/// The scaffolding every journal entry shares: today's timestamp and day, the org and the kind.
fn base_row(kind: &str, org: &str) -> SpendRow {
    SpendRow {
        ts: Utc::now().to_rfc3339(),
        day: today(),
        org: org.to_string(),
        kind: kind.to_string(),
        ..SpendRow::default()
    }
}

/// Stamps a colony-scoped row with the session it belongs to and the agent module that ran it. A
/// session record that never learned its module stays unnamed rather than an empty string; chat
/// rows never pass through here.
fn colony_row(mut row: SpendRow, s: &Session) -> SpendRow {
    row.session = Some(s.id.clone());
    row.agent = (!s.agent.is_empty()).then(|| s.agent.clone());
    row
}

/// Appends one row to the journal. A lost row is a lost measurement, not a reason to fail whatever
/// just happened, so a failed append is reported through the app's sticky storage alert and the
/// caller carries on — the same deal a lost routing record gets.
async fn append_row(app: &App, row: SpendRow) {
    let Ok(line) = serde_json::to_string(&row) else {
        return; // a fixed-shape row cannot fail to serialize
    };
    if let Err(e) = append_line(&spend_file(&app.cfg.data_dir), &line).await {
        app.storage_failed("append to the spend journal", &e).await;
    }
}

/// A colony was admitted, queued or starting: the journal's `launched` edge, filed today.
pub(crate) async fn record_launched(app: &App, s: &Session) {
    append_row(app, colony_row(base_row("launched", &s.org), s)).await;
}

/// A colony crossed into its terminal state — PR opened, merged or closed, nothing to push,
/// stopped or failed: the journal's `returned` edge. Recorded at the transition ([`App::update_session`]
/// and the queue's retire both call this), never on the updates that follow.
pub(crate) async fn record_returned(app: &App, s: &Session) {
    append_row(app, colony_row(base_row("returned", &s.org), s)).await;
}

/// The gateway routed and priced one response: its cost joins today's `routed` spend. There is no
/// model on the row: the gateway counts what the provider charged in total for a response and does
/// not know which of its models served it.
pub(crate) async fn record_routed(app: &App, s: &Session, cost: f64) {
    let mut row = colony_row(base_row("routed", &s.org), s);
    row.cost_usd = Some(cost);
    append_row(app, row).await;
}

/// One chat reply's tokens and cost (the cockpit's model chat, no colony): a `usage` row under the
/// conversation's workspace, or the `chat` pseudo-org when it has none, so it lands in spend history
/// beside the colonies' own. `cost` is `None` when the model's prices are unknown.
pub(crate) async fn record_chat_usage(
    app: &App,
    org: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cost: Option<f64>,
) {
    let mut row = base_row("usage", org);
    row.model = Some(model.to_string());
    row.input_tokens = input_tokens;
    row.output_tokens = output_tokens;
    row.cost_usd = cost;
    append_row(app, row).await;
}

/// What one turn's `model_usage` gained over the last cumulative, for the journal to record as
/// increments. `old_*` is a session's record before the turn, `new_*` after; a session that has
/// never reported something starts from zero, and a cost that only comes down again (a cheaper
/// re-estimate) writes no negative line.
struct TurnDelta {
    /// The models whose token totals grew, with each one's increment.
    models: Vec<(String, Tokens)>,
    /// How much the cost estimate grew this turn, once it is both known and positive.
    cost: Option<f64>,
}

fn turn_deltas(old_cost: Option<f64>, old_usage: Option<&Value>, new_cost: Option<f64>, new_usage: Option<&Value>) -> TurnDelta {
    let before = model_tokens(old_usage);
    let mut models = Vec::new();
    for (model, tokens) in model_tokens(new_usage) {
        let prior = before.iter().find(|(m, _)| *m == model).map(|(_, t)| *t).unwrap_or_default();
        let delta = tokens.saturating_sub(prior);
        if delta.total() > 0 {
            models.push((model.to_string(), delta));
        }
    }
    let cost = match (old_cost, new_cost) {
        (_, None) => None,
        (None, Some(cost)) => Some(cost),
        (Some(before), Some(after)) => (after > before).then_some(after - before),
    };
    // A measured 0.0 is a zero-dollar turn the same as an unmeasured one would look; either way
    // there is nothing to write. Same for a cost that only shrank.
    let cost = cost.filter(|c| *c > 0.0);
    TurnDelta { models, cost }
}

/// The journal rows one turn's delta files: a one-model turn's cost rides on that model's row; a
/// multi-model turn's cost has no single owner, so it goes on its own un-modeled row — per-model
/// cost rows there would have to split a dollar, which is fabrication. Cost alone, with nothing to
/// pin it to, files the same un-modeled row. Every row names the session and its harness.
fn usage_rows(day: &str, s: &Session, delta: &TurnDelta) -> Vec<SpendRow> {
    let usage = |model: &str, tokens: &Tokens, cost| {
        colony_row(
            SpendRow {
                ts: Utc::now().to_rfc3339(),
                day: day.to_string(),
                org: s.org.clone(),
                kind: "usage".into(),
                model: Some(model.to_string()),
                input_tokens: tokens.input,
                output_tokens: tokens.output,
                cache_read_tokens: tokens.cache_read,
                cache_write_tokens: tokens.cache_write,
                cost_usd: cost,
                ..SpendRow::default()
            },
            s,
        )
    };
    let mut rows = Vec::new();
    if delta.models.len() == 1 && delta.cost.is_some() {
        let (model, tokens) = &delta.models[0];
        rows.push(usage(model, tokens, delta.cost));
    } else {
        for (model, tokens) in &delta.models {
            rows.push(usage(model, tokens, None));
        }
        if let Some(cost) = delta.cost {
            rows.push(colony_row(
                SpendRow {
                    day: day.to_string(),
                    org: s.org.clone(),
                    kind: "usage".into(),
                    cost_usd: Some(cost),
                    ..SpendRow::default()
                },
                s,
            ));
        }
    }
    rows
}

/// Files one turn's spend delta: one `usage` row per model that grew, with this turn's cost where
/// the attribution rule lets it ride. `old_*` is what the session record held before the turn,
/// `new_*` what this event reported; the difference is what reaches the journal, so an append-only
/// file keeps summing to the same totals however many turns a colony took.
pub(crate) async fn record_turn_usage(
    app: &App,
    s: &Session,
    old_cost: Option<f64>,
    old_usage: Option<&Value>,
    new_cost: Option<f64>,
    new_usage: Option<&Value>,
) {
    let delta = turn_deltas(old_cost, old_usage, new_cost, new_usage);
    if delta.models.is_empty() && delta.cost.is_none() {
        return;
    }
    let day = today();
    for row in usage_rows(&day, s, &delta) {
        append_row(app, row).await;
    }
}

// ---------------------------------------------------------------------------
// The reader: `GET /api/spend/history`.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    days: Option<i64>,
}

/// `GET /api/spend/history?days=N`, the journal summed per org and day. Days default to 30 (at
/// most 365 are kept), come back oldest first, and only days the journal actually mentions appear.
/// The journal is an unbounded append-only file, so the whole read and sum move to the blocking
/// pool; the handler never holds the async executor over a row's worth of disk.
pub(crate) async fn history(State(app): State<Shared>, Query(query): Query<HistoryQuery>) -> Json<Value> {
    let days = query.days.unwrap_or(30).clamp(1, 365) as u32;
    let today = Utc::now().date_naive();
    let data_dir = app.cfg.data_dir.clone();
    let response = tokio::task::spawn_blocking(move || journal_days(&data_dir, today, days))
        .await
        .unwrap_or_default();
    Json(history_json(response))
}

/// The journal's `floor`..=`today` window as `YYYY-MM-DD` strings: the days a window of `days`
/// days ending today accepts. Counting starts at `today - (days - 1)`, so `days=1` is just today.
fn day_window(today: NaiveDate, days: u32) -> (String, String) {
    let floor = today
        .checked_sub_signed(chrono::Duration::days(days.saturating_sub(1) as i64))
        .unwrap_or(today);
    (floor.format("%Y-%m-%d").to_string(), today.format("%Y-%m-%d").to_string())
}

/// Reads the journal's rows streamed through a `BufReader`, one line at a time, so an unboundedly
/// old append-only file never materializes in memory. A row the window would never answer — its day
/// outside `floor`..=`today` — drops as it passes, and so does anything that would break a row: a
/// malformed or torn line, an empty tail, or a row whose future keys its `#[serde(default)]`
/// already absorbs. A file that is not there is not a failure: an install that has never run a
/// colony has no spend.
fn read_journal(data_dir: &Path, floor: &str, today: &str) -> Vec<SpendRow> {
    let Ok(file) = std::fs::File::open(spend_file(data_dir)) else {
        return Vec::new();
    };
    let floor = floor.to_owned();
    let today = today.to_owned();
    std::io::BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
        .filter_map(|line| {
            // UTF-8 is checked per line, as the routing ledger's reader does: a torn multi-byte
            // line costs that line and not the rest of the file.
            let row = serde_json::from_str::<SpendRow>(std::str::from_utf8(&line).ok()?).ok()?;
            (!row.day.is_empty() && !row.org.is_empty() && row.day >= floor && row.day <= today).then_some(row)
        })
        .collect()
}

/// One org's row in one day of the history: its spend plus how many colonies ran that day.
#[derive(Clone, Debug, Default)]
struct DayOrg {
    spend: OrgSpend,
    launched: u64,
    returned: u64,
}

/// Sums the journal like the live org list would, plus the day's `launched` and `returned` counts,
/// grouped by day (oldest first) and org (by name) — the two orderings the history answers sorted
/// in. Days outside the window are skipped, and a row whose kind a newer build added is ignored
/// rather than fatal. [`read_journal`] already dropped the out-of-window rows on the way in; the
/// check stays here for any caller that hands over a fuller journal in hand.
fn aggregate(rows: &[SpendRow], today: NaiveDate, days: u32) -> Vec<(String, Vec<(String, DayOrg)>)> {
    let (floor, today) = day_window(today, days);
    let mut days_map: BTreeMap<String, BTreeMap<String, DayOrg>> = BTreeMap::new();
    for row in rows {
        if row.day.is_empty() || row.org.is_empty() || row.day < floor || row.day > today {
            continue;
        }
        let org = days_map
            .entry(row.day.clone())
            .or_default()
            .entry(row.org.clone())
            .or_default();
        let spend = &mut org.spend;
        match row.kind.as_str() {
            "usage" => {
                let tokens = Tokens {
                    input: row.input_tokens,
                    output: row.output_tokens,
                    cache_read: row.cache_read_tokens,
                    cache_write: row.cache_write_tokens,
                };
                spend.add_tokens(tokens);
                spend.add_cost(row.cost_usd);
                if let Some(model) = row.model.as_deref()
                    && !model.is_empty()
                {
                    spend.add_model(model, tokens, row.cost_usd);
                }
            }
            "routed" => spend.add_routed_from(row.cost_usd),
            "launched" => org.launched += 1,
            "returned" => org.returned += 1,
            _ => {}
        }
    }
    days_map
        .into_iter()
        .map(|(day, orgs)| (day, orgs.into_iter().collect()))
        .collect()
}

/// Sums the journal for one window the way `GET /api/spend/history` answers it: [`read_journal`]
/// streams and windows the rows, [`aggregate`] does the summing. A separate entry point so the
/// blocking read and sum can move to the blocking pool as one unit, and tests can ask for any
/// window without an HTTP round trip.
fn journal_days(data_dir: &Path, today: NaiveDate, days: u32) -> Vec<(String, Vec<(String, DayOrg)>)> {
    let (floor, today_str) = day_window(today, days);
    let rows = read_journal(data_dir, &floor, &today_str);
    aggregate(&rows, today, days)
}

/// The `{"days": [...]}` document the endpoint answers with.
fn history_json(days: Vec<(String, Vec<(String, DayOrg)>)>) -> Value {
    json!({"days": days.into_iter().map(|(day, orgs)| {
        let orgs = orgs
            .into_iter()
            .map(|(org, row)| {
                let mut entry = spend_json(&row.spend);
                entry["org"] = Value::String(org);
                entry["launched"] = json!(row.launched);
                entry["returned"] = json!(row.returned);
                entry
            })
            .collect::<Vec<Value>>();
        json!({"day": day, "orgs": orgs})
    })
    .collect::<Vec<Value>>()})
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{SessionStatus, tests::colony};
    use serde_json::json;

    #[test]
    fn rollup_sums_sessions_and_attributes_cost_by_the_single_model_rule() {
        // One session on a single model, fully measured.
        let mut single = colony("acme", SessionStatus::Running);
        single.cost_usd = Some(4.0);
        single.routed_cost_usd = Some(1.0);
        single.model_usage = Some(
            json!({"claude-opus-5": {"input_tokens": 100, "output_tokens": 50, "cache_read_tokens": 25, "cache_write_tokens": 5}}),
        );

        // A second session spread over two models; its cost must land nowhere.
        let mut multi = colony("acme", SessionStatus::Running);
        multi.cost_usd = Some(2.0);
        multi.model_usage = Some(json!({
            "claude-opus-5": {"output_tokens": 10},
            "deepseek/deepseek-flash": {"input_tokens": 200},
        }));

        // A subscription (Claude Max) session: measured nothing, and must not read as free zeros.
        let mut free = colony("acme", SessionStatus::PrOpened);
        free.cost_usd = None;
        free.routed_cost_usd = None;
        free.model_usage = None;

        let spend = rollup_sessions([&single, &multi, &free]);
        assert_eq!(spend.cost_usd, Some(6.0), "4.0 measured + 2.0 measured");
        assert_eq!(spend.routed_cost_usd, Some(1.0));
        assert_eq!(spend.input_tokens, 300);
        assert_eq!(spend.output_tokens, 60);
        assert_eq!(spend.cache_read_tokens, 25);
        assert_eq!(spend.cache_write_tokens, 5);

        let value = spend_json(&spend);
        assert_eq!(value["cost_usd"], json!(6.0));
        let models = value["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        // flash: 200; opus: 100+50+25+5 (single) + 10 (multi) = 190. Largest first.
        assert_eq!(models[0]["model"], "deepseek/deepseek-flash");
        assert_eq!(models[0]["tokens"], json!(200));
        assert_eq!(
            models[0]["cost_usd"],
            Value::Null,
            "the multi-model session's cost is attributed nowhere"
        );
        assert_eq!(models[1]["model"], "claude-opus-5");
        assert_eq!(models[1]["tokens"], json!(190));
        assert_eq!(
            models[1]["cost_usd"],
            json!(4.0),
            "the single-model session's Claude-reported cost; routed dollars never ride on a model"
        );

        // An org with nothing measured reports nulls and empty models, not zeros.
        let free_org = rollup_sessions([&free]);
        assert_eq!(free_org.cost_usd, None);
        assert_eq!(free_org.routed_cost_usd, None);
        assert_eq!(free_org.input_tokens, 0);
        let value = spend_json(&free_org);
        assert_eq!(value["cost_usd"], Value::Null);
        assert_eq!(value["models"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn turn_deltas_report_the_increment_not_the_cumulative() {
        let old = json!({"claude-opus-5": {"input_tokens": 100, "output_tokens": 50}});
        let new = json!({"claude-opus-5": {"input_tokens": 400, "output_tokens": 60}});
        let delta = turn_deltas(Some(1.0), Some(&old), Some(2.5), Some(&new));
        assert_eq!(delta.cost, Some(1.5), "2.5 − 1.0, not the new cumulative");
        let (model, tokens) = &delta.models[0];
        assert_eq!(model, "claude-opus-5");
        assert_eq!(tokens.input, 300);
        assert_eq!(tokens.output, 10);

        // A first measurement is the whole value; a model new to the session starts from zero.
        let first = turn_deltas(None, None, Some(0.4), Some(&json!({"m": {"output_tokens": 7}})));
        assert_eq!(first.cost, Some(0.4));
        assert_eq!(first.models[0].1.output, 7);

        // A cheaper re-estimate floors to nothing: no negative rows, no cost rows for zero gain.
        let flat = json!({"m": {"input_tokens": 500}});
        let lower = turn_deltas(Some(2.0), Some(&flat), Some(1.5), Some(&flat));
        assert_eq!(lower.cost, None);
        assert!(lower.models.is_empty());
    }

    #[test]
    fn a_multi_model_turn_files_cost_on_its_own_row() {
        let mut s = colony("acme", SessionStatus::Running);
        s.id = "col-1".into();
        s.agent = "claude-code".into();
        let delta = turn_deltas(
            None,
            None,
            Some(3.0),
            Some(&json!({"a": {"input_tokens": 10}, "b": {"input_tokens": 20}})),
        );
        let rows = usage_rows("2026-09-20", &s, &delta);
        assert_eq!(rows.len(), 3, "one row per model plus the unattributable cost");
        assert!(
            rows.iter()
                .all(|r| r.session.as_deref() == Some("col-1") && r.agent.as_deref() == Some("claude-code")),
            "every row of the turn names the colony and its harness"
        );
        let model_rows: Vec<&SpendRow> = rows.iter().filter(|r| r.model.is_some()).collect();
        let cost_rows: Vec<&SpendRow> = rows.iter().filter(|r| r.model.is_none()).collect();
        assert_eq!(model_rows.len(), 2);
        assert!(
            model_rows.iter().all(|r| r.cost_usd.is_none()),
            "a split dollar is never fabricated"
        );
        assert_eq!(cost_rows.len(), 1);
        assert_eq!(cost_rows[0].cost_usd, Some(3.0));

        // A single-model turn carries its cost on that one model's row.
        let single = turn_deltas(None, None, Some(1.5), Some(&json!({"a": {"input_tokens": 5}})));
        let rows = usage_rows("2026-09-20", &s, &single);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model.as_deref(), Some("a"));
        assert_eq!(rows[0].cost_usd, Some(1.5));

        // Cost alone, with tokens flat, is one un-modeled row.
        let flat = json!({"a": {"input_tokens": 5}});
        let only_cost = turn_deltas(Some(1.0), Some(&flat), Some(2.0), Some(&flat));
        let rows = usage_rows("2026-09-20", &s, &only_cost);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, None);
        assert_eq!(rows[0].cost_usd, Some(1.0));
    }

    #[tokio::test]
    async fn journal_rows_group_by_day_and_survive_malformed_and_out_of_window_lines() {
        let root = std::env::temp_dir().join(format!("colonizer-spend-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let today = Utc::now().date_naive().format("%Y-%m-%d").to_string();
        let mut acme = colony("acme", SessionStatus::Running);
        acme.id = "col-1".into();
        acme.agent = "claude-code".into();
        let mut team = colony("team", SessionStatus::Running);
        team.id = "col-2".into();

        // A day's activity, filed through the same helpers every other path calls.
        record_launched(&app, &acme).await;
        record_launched(&app, &acme).await;
        record_returned(&app, &acme).await;
        record_launched(&app, &team).await;
        record_routed(&app, &acme, 0.25).await;
        record_turn_usage(
            &app,
            &acme,
            None,
            None,
            Some(1.0),
            Some(&json!({"claude-opus-5": {"input_tokens": 100}})),
        )
        .await;
        // The second turn's cumulative replaced the record; the journal gets only the increment.
        record_turn_usage(
            &app,
            &acme,
            Some(1.0),
            Some(&json!({"claude-opus-5": {"input_tokens": 100}})),
            Some(2.5),
            Some(&json!({"claude-opus-5": {"input_tokens": 400, "output_tokens": 10}})),
        )
        .await;

        // Rows a healthy journal never has, announcing how the reader copes: an old day out of the
        // window, a kind a newer build might add, and a line that is not JSON at all.
        let file = spend_file(&app.cfg.data_dir);
        let mut raw = std::fs::read_to_string(&file).unwrap();
        let mut outside = base_row("returned", "acme");
        outside.day = "2000-01-01".into();
        raw.push_str(&serde_json::to_string(&outside).unwrap());
        raw.push('\n');
        raw.push_str(&serde_json::to_string(&base_row("harvested", "acme")).unwrap());
        raw.push('\n');
        raw.push_str("this is not a journal row\n");
        std::fs::write(&file, raw).unwrap();

        // 7 helper rows + 2 hand-written ones; the garbage line never parsed. A wide floor keeps the
        // 2000-01-01 row in hand so it is the aggregation, not the read, that drops it; the
        // narrow-window behaviour is the dedicated test below.
        let rows = read_journal(&app.cfg.data_dir, "0000-01-01", &today);
        assert_eq!(rows.len(), 9);

        let days = aggregate(&rows, Utc::now().date_naive(), 30);
        assert_eq!(days.len(), 1, "one day has rows");
        let (day, orgs) = &days[0];
        assert_eq!(day, &today);
        assert_eq!(orgs.len(), 2);
        let acme = orgs.iter().find(|(org, _)| org == "acme").unwrap();
        let team = orgs.iter().find(|(org, _)| org == "team").unwrap();
        assert_eq!(acme.1.launched, 2);
        assert_eq!(acme.1.returned, 1, "the 2000-01-01 row is out of the window");
        assert_eq!(team.1.launched, 1);
        assert_eq!(acme.1.spend.cost_usd, Some(2.5), "1.0 + this turn's 1.5, both deltas");
        assert_eq!(acme.1.spend.routed_cost_usd, Some(0.25));
        assert_eq!(acme.1.spend.input_tokens, 400);
        assert_eq!(acme.1.spend.output_tokens, 10);
        let opus = acme.1.spend.models.get("claude-opus-5").unwrap();
        assert_eq!(opus.tokens, 410);
        assert_eq!(opus.cost_usd, Some(2.5));
        assert!(team.1.spend.models.is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn history_answers_in_the_contracts_shape_and_clamps_days() {
        let root = std::env::temp_dir().join(format!("colonizer-spend-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let mut acme = colony("acme", SessionStatus::Running);
        acme.id = "col-1".into();
        record_launched(&app, &acme).await;
        record_launched(&app, &acme).await;
        record_returned(&app, &acme).await;
        record_launched(&app, &colony("team", SessionStatus::Running)).await;
        record_routed(&app, &acme, 0.5).await;
        record_turn_usage(
            &app,
            &acme,
            None,
            None,
            Some(3.0),
            Some(&json!({"claude-opus-5": {"input_tokens": 200}})),
        )
        .await;

        let Json(out) = history(State(app.clone()), Query(HistoryQuery { days: Some(0) })).await;
        let days = out["days"].as_array().unwrap();
        assert_eq!(days.len(), 1, "days=0 clamps up to 1 and today still answers");
        let orgs = days[0]["orgs"].as_array().unwrap();
        let acme = orgs.iter().find(|o| o["org"] == "acme").unwrap();
        assert_eq!(acme["launched"], json!(2));
        assert_eq!(acme["returned"], json!(1));
        assert_eq!(acme["cost_usd"], json!(3.0));
        assert_eq!(acme["routed_cost_usd"], json!(0.5));
        assert_eq!(acme["tokens"]["input"], json!(200));
        assert_eq!(acme["models"][0]["model"], "claude-opus-5");
        assert_eq!(acme["models"][0]["tokens"], json!(200));
        assert_eq!(acme["models"][0]["cost_usd"], json!(3.0));
        let team = orgs.iter().find(|o| o["org"] == "team").unwrap();
        assert_eq!(team["launched"], json!(1));
        assert_eq!(team["cost_usd"], Value::Null);

        // days=999 clamps down to 365; the same single day still answers.
        let Json(out) = history(State(app.clone()), Query(HistoryQuery { days: Some(999) })).await;
        assert_eq!(out["days"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn routed_dollars_ride_the_totals_never_a_model_row() {
        // A single-model session the gateway priced but Claude did not estimate: the org totals
        // carry the routed dollar, and the model's cost stays null — on both the live rollup and
        // the journal's history, so the two surfaces cannot diverge on what a model's cost means
        // (Claude's own measurement only).
        let root = std::env::temp_dir().join(format!("colonizer-spend-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);

        // The session-side picture.
        let mut routed = colony("acme", SessionStatus::Running);
        routed.cost_usd = None;
        routed.routed_cost_usd = Some(0.5);
        routed.model_usage = Some(json!({"deepseek/deepseek-flash": {"input_tokens": 200}}));
        let spend = rollup_sessions([&routed]);
        assert_eq!(spend.cost_usd, None);
        assert_eq!(spend.routed_cost_usd, Some(0.5));
        let value = spend_json(&spend);
        let models = value["models"].as_array().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0]["cost_usd"],
            Value::Null,
            "a routed dollar is not a model's cost on the rollup"
        );

        // The same session filed as its journal rows: a `routed` row for the dollar, a `usage`
        // row for the model with no cost to ride on it. The history sums to the same picture.
        record_routed(&app, &routed, 0.5).await;
        record_turn_usage(
            &app,
            &routed,
            None,
            None,
            None, // Claude reported no cost for this routed turn
            Some(&json!({"deepseek/deepseek-flash": {"input_tokens": 200}})),
        )
        .await;
        let days = journal_days(&app.cfg.data_dir, Utc::now().date_naive(), 30);
        assert_eq!(days.len(), 1);
        let (_, orgs) = &days[0];
        let (_, acme) = orgs.iter().find(|(org, _)| org == "acme").unwrap();
        assert_eq!(acme.spend.cost_usd, None);
        assert_eq!(acme.spend.routed_cost_usd, Some(0.5));
        let model = acme.spend.models.get("deepseek/deepseek-flash").unwrap();
        assert_eq!(model.tokens, 200);
        assert_eq!(
            model.cost_usd, None,
            "a routed dollar is not a model's cost on the journal either"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_row_older_than_the_window_is_dropped_at_read_time() {
        let root = std::env::temp_dir().join(format!("colonizer-spend-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let today = Utc::now().date_naive();

        // A usage row from a fortnight ago, filed under that week's day.
        let mut old = base_row("usage", "acme");
        old.day = today
            .checked_sub_signed(chrono::Duration::days(14))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();
        old.model = Some("claude-opus-5".into());
        old.input_tokens = 100;
        old.cost_usd = Some(1.0);
        append_row(&app, old).await;

        let days = journal_days(&app.cfg.data_dir, today, 5);
        assert!(days.is_empty(), "a row outside the window never reaches the aggregation");

        // The same journal read with a window wide enough lands the row.
        let days = journal_days(&app.cfg.data_dir, today, 20);
        assert_eq!(days.len(), 1);
        let (_, orgs) = &days[0];
        let (_, acme) = orgs.iter().find(|(org, _)| org == "acme").unwrap();
        assert_eq!(acme.spend.cost_usd, Some(1.0));
        assert_eq!(acme.spend.models.get("claude-opus-5").unwrap().tokens, 100);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn colony_rows_name_their_session_and_harness_chat_rows_neither() {
        let root = std::env::temp_dir().join(format!("colonizer-spend-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let mut s = colony("acme", SessionStatus::Running);
        s.id = "col-1".into();
        s.agent = "claude-code".into();

        record_launched(&app, &s).await;
        record_routed(&app, &s, 0.25).await;
        record_turn_usage(
            &app,
            &s,
            None,
            None,
            Some(1.0),
            Some(&json!({"claude-opus-5": {"input_tokens": 100}})),
        )
        .await;
        // A record that never learned its module is unnamed, not an empty string.
        let mut unnamed = colony("acme", SessionStatus::Running);
        unnamed.id = "col-2".into();
        record_returned(&app, &unnamed).await;
        // The cockpit's chat spend is nobody's colony: no session, no harness.
        record_chat_usage(&app, "chat", "claude-opus-5", 10, 5, None).await;

        let today = Utc::now().date_naive().format("%Y-%m-%d").to_string();
        let rows = read_journal(&app.cfg.data_dir, "0000-01-01", &today);
        assert_eq!(rows.len(), 5);
        let named: Vec<&SpendRow> = rows.iter().filter(|r| r.session.as_deref() == Some("col-1")).collect();
        assert_eq!(named.len(), 3, "launched, routed and the turn's usage row");
        assert!(
            named.iter().all(|r| r.agent.as_deref() == Some("claude-code")),
            "every colony-scoped row names the harness that ran it"
        );
        let unnamed_row = rows.iter().find(|r| r.session.as_deref() == Some("col-2")).unwrap();
        assert_eq!(unnamed_row.agent, None);
        let chat = rows.iter().find(|r| r.org == "chat").unwrap();
        assert_eq!(chat.session, None, "a chat row is nobody's colony");
        assert_eq!(chat.agent, None);

        // The new fields ride along without changing what the history sums.
        let days = aggregate(&rows, Utc::now().date_naive(), 30);
        let (_, orgs) = &days[0];
        let (_, acme) = orgs.iter().find(|(org, _)| org == "acme").unwrap();
        assert_eq!(acme.spend.cost_usd, Some(1.0));
        assert_eq!(acme.spend.routed_cost_usd, Some(0.25));
        assert_eq!(acme.spend.input_tokens, 100);

        // Omitted, not empty, on the wire: the chat line carries neither key.
        let raw = std::fs::read_to_string(spend_file(&app.cfg.data_dir)).unwrap();
        let chat_line = raw.lines().find(|l| l.contains("chat")).unwrap();
        assert!(!chat_line.contains("session") && !chat_line.contains("agent"), "{chat_line}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rows_from_before_session_and_agent_still_parse_and_aggregate() {
        let root = std::env::temp_dir().join(format!("colonizer-spend-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let day = Utc::now().date_naive().format("%Y-%m-%d").to_string();
        // The spelling a build before the fields wrote: no session, no agent on any row.
        let row = |kind: &str, extra: &str| {
            format!(
                r#"{{"ts":"{}","day":"{day}","org":"acme","kind":"{kind}"{extra}}}"#,
                Utc::now().to_rfc3339()
            )
        };
        let legacy = format!(
            "{}\n{}\n{}\n",
            row(
                "usage",
                r#","model":"claude-opus-5","input_tokens":100,"output_tokens":10,"cost_usd":1.0"#
            ),
            row("routed", r#","cost_usd":0.5"#),
            row("launched", ""),
        );
        std::fs::write(spend_file(&app.cfg.data_dir), legacy).unwrap();

        let rows = read_journal(&app.cfg.data_dir, "0000-01-01", &day);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.session.is_none() && r.agent.is_none()));
        let days = journal_days(&app.cfg.data_dir, Utc::now().date_naive(), 30);
        assert_eq!(days.len(), 1);
        let (_, orgs) = &days[0];
        let (_, acme) = orgs.iter().find(|(org, _)| org == "acme").unwrap();
        assert_eq!(acme.spend.cost_usd, Some(1.0));
        assert_eq!(acme.spend.routed_cost_usd, Some(0.5));
        assert_eq!(acme.spend.input_tokens, 100);
        assert_eq!(acme.spend.output_tokens, 10);
        assert_eq!(acme.launched, 1);
        assert_eq!(acme.spend.models.get("claude-opus-5").unwrap().tokens, 110);

        let _ = std::fs::remove_dir_all(root);
    }

    /// Reconciliation with the node half of issue #296: the shared fixture is read and summed the
    /// exact way `GET /api/spend/history` reads and sums a `spend.jsonl`, and the sums must equal
    /// the constants `scripts/test/colony-report.test.mjs` asserts for the same file (its
    /// `--costs` reconciliation test). Change the fixture, or either side's expectations, and the
    /// other two follow.
    #[test]
    fn the_shared_spend_fixture_sums_the_same_on_both_sides_of_the_language_split() {
        let root = std::env::temp_dir().join(format!("colonizer-spend-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        std::fs::write(
            spend_file(&app.cfg.data_dir),
            include_str!("../../../scripts/test/fixtures/spend-costs.jsonl"),
        )
        .unwrap();

        // A fixed window wide enough for every day the fixture names (2026-09-14 and 2026-09-15),
        // so the test does not age out as the real calendar moves.
        let today = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        let days = journal_days(&app.cfg.data_dir, today, 30);
        assert_eq!(days.len(), 2, "the two days the fixture mentions, oldest first");
        let (mut cost, mut routed, mut input, mut output, mut cache_read, mut cache_write) = (0.0, 0.0, 0, 0, 0, 0);
        for (_, orgs) in &days {
            for (_, org) in orgs {
                cost += org.spend.cost_usd.unwrap_or_default();
                routed += org.spend.routed_cost_usd.unwrap_or_default();
                input += org.spend.input_tokens;
                output += org.spend.output_tokens;
                cache_read += org.spend.cache_read_tokens;
                cache_write += org.spend.cache_write_tokens;
            }
        }
        // Every dollar the fixture carries is a binary fraction, so the f64 sums are exact; the
        // epsilon keeps the comparison honest should the fixture ever grow one that is not.
        assert!(
            (cost - 1.1875).abs() < 1e-9,
            "usage-row dollars, chat and legacy included, got {cost}"
        );
        assert!((routed - 0.375).abs() < 1e-9, "the routed/gateway dollars, got {routed}");
        assert_eq!(input, 1300);
        assert_eq!(output, 305);
        assert_eq!(cache_read, 200);
        assert_eq!(cache_write, 0);

        // The new-format rows name their colony and harness; the fixture's one legacy row and its
        // chat row still parse, unnamed.
        let rows = read_journal(&app.cfg.data_dir, "0000-01-01", "2026-09-30");
        assert_eq!(rows.len(), 17, "the torn line is skipped, everything else reads");
        assert!(
            rows.iter()
                .filter(|r| r.session.as_deref() == Some("claudeaa"))
                .all(|r| r.agent.as_deref() == Some("claude-code")),
            "the claudeaa rows carry their harness"
        );
        let legacy = rows.iter().find(|r| r.session.is_none() && r.day == "2026-09-14").unwrap();
        assert_eq!(legacy.agent, None, "the fixture's legacy row parses unnamed");
        let chat = rows.iter().find(|r| r.org == "chat").unwrap();
        assert_eq!(chat.session, None);
        assert_eq!(chat.agent, None);

        let _ = std::fs::remove_dir_all(root);
    }
}
