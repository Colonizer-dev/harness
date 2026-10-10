//! Per-turn model routing, in shadow (issue #1152). The boot-time decision (`routing.rs`) picks one
//! tier for a colony's whole task; this module re-asks at every orchestrator turn end — the way
//! claude-router would — what tier the colony's *current* turn needs and what moving to it would
//! cost, and appends the answer to the colony's `turn_routing.jsonl`. Nothing here changes a model:
//! the record is a measurement, and whether per-turn routing ever acts is decided from the data it
//! accumulates. What act mode *would* do at this turn end is evaluated in shadow too, so the record
//! carries a verdict, not just an opinion.
//!
//! Two properties hold by construction. Subagents are never routed: [`on_turn_end`] takes whose turn
//! ended and returns before anything else for a subagent's. And the simulated act-mode state — the
//! tier the colony would be on, the turns since its last switch — is read back from the previous
//! record, so the shadow keeps no state of its own: a lost log line costs one turn of hysteresis
//! memory, nothing else.

use crate::providers::{self, Pricing};
use crate::routing::Tier;
use crate::store::SessionStore;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::time::Duration;

/// How many recent user/assistant texts join the current prompt in the classifier's input: long
/// enough to see what the colony is doing, short enough to bound what one turn end reads.
pub(crate) const RECENT_TEXTS: usize = 6;

/// The most characters of one text the classifier sees, counted on char boundaries. A longer text is
/// cut here, before it can dominate the rule's body-size score on its length alone.
pub(crate) const TEXT_CAP: usize = 1_200;

/// A downgrade within this many turns of the previous (simulated) switch is held, however good the
/// dollar case: flipping tiers every turn would defeat the cache both ways.
pub(crate) const HYSTERESIS_TURNS: u32 = 3;

/// The bound on one per-turn classification, the Jev ask included — the only part of it that can
/// wait on anything. Past it the current tier stands and the record says so.
const CLASSIFY_TIMEOUT: Duration = Duration::from_millis(1500);

/// A Claude session window this close to its cap makes a downgrade worth it even when the dollar
/// estimate says no: stretching the subscription beats stopping at the cap (issue #1152).
const NEAR_LIMIT_PCT: f64 = 80.0;

/// The per-colony ledger's name, and how much of it one turn end reads back to place itself.
const FILE: &str = "turn_routing.jsonl";
const TAIL_BYTES: u64 = 16 * 1024;

/// Who said a text, so the classifier's input keeps the distinction the conversation had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    User,
    Assistant,
}

impl Side {
    fn label(self) -> &'static str {
        match self {
            Side::User => "user",
            Side::Assistant => "assistant",
        }
    }
}

/// One typed entry of the colony's conversation. A turn produces far more than prose — tool calls,
/// results, thinking, status — and none of it is routed on: everything that is not user or
/// assistant text arrives as [`Entry::Other`] and is dropped before the input is bounded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Text(Side, String),
    Other,
}

/// The classifier's whole input: the current prompt plus up to [`RECENT_TEXTS`] most-recent
/// user/assistant texts, oldest first, each capped at [`TEXT_CAP`] characters and prefixed with who
/// said it. Deliberately no system prompt and no tool input or result — the tier a turn needs is
/// judged on the conversation, not on what was read with it.
pub(crate) fn bounded_input(current: &str, recent: impl Iterator<Item = Entry>) -> String {
    let mut window: Vec<(Side, String)> = Vec::new();
    for entry in recent {
        let Entry::Text(side, text) = entry else { continue };
        if window.len() == RECENT_TEXTS {
            window.remove(0);
        }
        window.push((side, cap(&text).to_string()));
    }
    window
        .iter()
        .map(|(side, text)| format!("{}: {text}", side.label()))
        .chain(std::iter::once(format!("user: {}", cap(current))))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text cut to at most [`TEXT_CAP`] characters without splitting one.
fn cap(text: &str) -> &str {
    match text.char_indices().nth(TEXT_CAP) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// Why the record's suggested tier is what it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TurnSource {
    /// The rule's own tier (`routing::decide`).
    Rule,
    /// Jev's confident opinion, applied the way act mode would apply it.
    Jev,
    /// The classification did not finish inside [`CLASSIFY_TIMEOUT`]; the current tier stands.
    Timeout,
    /// The Jev ask failed; the current tier stands.
    Error,
}

/// What act mode would do at this turn end, decided in shadow so the record carries the action, not
/// just the suggestion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    /// The suggested tier is the one already in force.
    Stay,
    /// Act mode would move the colony to the suggested tier.
    Switch,
    /// A downgrade inside the hysteresis window since the previous switch.
    HeldByHysteresis,
    /// A downgrade the cache refill outweighs over the hysteresis window.
    CacheRefillExceedsSaving,
    /// A downgrade whose dollar case could not be run: neither model is priced. Held, unless the
    /// account is near its session limit, where the downgrade does not need the dollar case.
    Unpriced,
}

/// The verdict with the two dollar figures behind it. Both are `None` whenever the figures could not
/// be estimated — an unpriced model is not a zero-dollar one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct Act {
    verdict: Verdict,
    would_switch: bool,
    est_refill_usd: Option<f64>,
    est_saving_usd: Option<f64>,
}

/// What a switch is judged on, in dollars: loading the context on the target model once — its first
/// read is a cache write where the provider writes cache, uncached input where it does not, so the
/// dearer of the two rates — against what the next [`HYSTERESIS_TURNS`] turns save reading that same
/// context from the target's cache instead of the current model's. Only the context is priced: it is
/// the one term the turn's own usage measures, and the term that decides whether a downgrade pays.
fn estimates(current_pricing: Pricing, target_pricing: Pricing, context_tokens: u64) -> (f64, f64) {
    let context = context_tokens as f64;
    let refill = context * target_pricing.input_per_mtok.max(target_pricing.cache_write_per_mtok) / 1_000_000.0;
    let saving = context * (current_pricing.cache_read_per_mtok - target_pricing.cache_read_per_mtok) * HYSTERESIS_TURNS as f64
        / 1_000_000.0;
    (refill, saving)
}

fn switch(priced: Option<(f64, f64)>) -> Act {
    Act {
        verdict: Verdict::Switch,
        would_switch: true,
        est_refill_usd: priced.map(|(refill, _)| refill),
        est_saving_usd: priced.map(|(_, saving)| saving),
    }
}

fn held(verdict: Verdict, priced: Option<(f64, f64)>) -> Act {
    Act {
        verdict,
        would_switch: false,
        est_refill_usd: priced.map(|(refill, _)| refill),
        est_saving_usd: priced.map(|(_, saving)| saving),
    }
}

/// The act-mode policy, evaluated in shadow: would a switch at this turn end pay? An upgrade is
/// never second-guessed — capability first. A downgrade needs to be outside the hysteresis window
/// and to beat the cache refill over the next [`HYSTERESIS_TURNS`] turns, unless the Claude account
/// is near its session limit, where stretching the subscription is worth more than the dollars.
/// Pure, so the verdict can be trusted apart from everything that feeds it.
pub(crate) fn policy(
    current: Tier,
    suggested: Tier,
    turns_since_switch: u32,
    context_tokens: u64,
    current_pricing: Option<Pricing>,
    target_pricing: Option<Pricing>,
    account_near_limit: bool,
) -> Act {
    if suggested == current {
        return held(Verdict::Stay, None);
    }
    let priced = current_pricing
        .zip(target_pricing)
        .map(|(now, next)| estimates(now, next, context_tokens));
    if suggested > current {
        return switch(priced);
    }
    if turns_since_switch < HYSTERESIS_TURNS {
        return held(Verdict::HeldByHysteresis, priced);
    }
    match priced {
        Some((refill, saving)) if saving > refill => switch(Some((refill, saving))),
        // Unpriced, or priced and not worth it: only a near-limit account downgrades anyway.
        _ if account_near_limit => switch(priced),
        Some(priced) => held(Verdict::CacheRefillExceedsSaving, Some(priced)),
        None => held(Verdict::Unpriced, None),
    }
}

/// One line of `turn_routing.jsonl`. The tiers are spelled the way settings spell them.
#[derive(Serialize)]
struct TurnRecord {
    ts: DateTime<Utc>,
    session: String,
    /// How many turn ends this colony has recorded, zero-based.
    turn: u64,
    /// The model that actually ran the turn.
    model: String,
    /// The tier the simulated act mode has the colony on at this turn's end.
    current_tier: String,
    /// The tier the per-turn classification picked for this turn.
    suggested_tier: String,
    source: TurnSource,
    verdict: Verdict,
    would_switch: bool,
    /// The context the turn that just ran held: its input plus both cache kinds.
    context_tokens: u64,
    est_refill_usd: Option<f64>,
    est_saving_usd: Option<f64>,
    account_near_limit: bool,
}

/// The one place a turn end is routed. Spawned from the event loop, so it never delays one: the
/// classification reads the colony's own logs, may ask Jev, and appends one record — all after the
/// turn is already over. `subagent` is whose turn ended; a subagent's turn is its own and is never
/// routed, so only an orchestrator turn end gets past this line.
pub(crate) async fn on_turn_end(
    app: crate::Shared,
    id: String,
    subagent: bool,
    old_usage: Option<Value>,
    new_usage: Option<Value>,
) {
    if subagent {
        return;
    }
    // The turn's own shape is what the cumulative `model_usage` grew by; a turn that grew nothing —
    // a synthetic end, a runner that never reported — has no model and no context to route on.
    let Some((model, tokens)) = turn_delta(old_usage.as_ref(), new_usage.as_ref()) else {
        return;
    };
    let Some(s) = app.session(&id).await else { return };
    let modules = app.modules.read().await.clone();
    let org = app.org_settings(&s.org);
    let Some(agent) = app.agents.iter().find(|a| a.id == s.agent) else {
        return;
    };
    let choice = crate::orgs::effective_agent_for(&modules, &org, &agent.id);
    let setting = |key: &str| crate::config::setting(&choice, &agent.schema, key).cloned();
    let flag = |key: &str| setting(key).and_then(|v| v.as_bool());
    if !flag("turn_route_shadow").unwrap_or(true) {
        return;
    }
    let model_low = crate::config::setting_str(&choice, &agent.schema, "model_low");
    let model_medium = crate::config::setting_str(&choice, &agent.schema, "model");
    let model_high = crate::config::setting_str(&choice, &agent.schema, "model_high");
    let jev_mode = crate::routing::JevMode::from_settings(
        flag("jev_routing_act").unwrap_or(false),
        flag("jev_shadow_mode").unwrap_or(false),
    );
    let jev_act_confidence = setting("jev_routing_act_confidence").and_then(|v| v.as_f64()).unwrap_or(0.8);

    // The simulated act-mode state: the boot tier until this ledger says a switch would have moved
    // it, and the turns since that switch.
    let boot_tier = s
        .model_routing
        .as_ref()
        .and_then(|r| r["tier"].as_str())
        .and_then(Tier::parse)
        .unwrap_or(Tier::Medium);
    let records = record_tail(app.store(), &id).await;
    let turns_since_switch = records.iter().rev().take_while(|r| r["would_switch"] != true).count() as u32;
    let (turn, sim_tier) = match records.last() {
        Some(last) => {
            let switched = last["would_switch"] == true;
            let tier = Tier::parse(
                last[if switched { "suggested_tier" } else { "current_tier" }]
                    .as_str()
                    .unwrap_or_default(),
            )
            .unwrap_or(boot_tier);
            (last["turn"].as_u64().unwrap_or(0) + 1, tier)
        }
        None => (0, boot_tier),
    };

    // The bounded conversation, read off the colony's own event log: the current prompt is its last
    // user message, the window is what came before it. Without a prompt there is nothing to classify.
    let events = crate::diagnosis::tail_events(app.store(), &id).await;
    let Some((current, recent)) = prompt_and_recent(&events) else {
        return;
    };
    let (suggested, source) = classify(
        &bounded_input(&current, recent),
        sim_tier,
        jev_mode,
        org.jev != Some(false),
        jev_act_confidence,
    )
    .await;

    // A suggestion that lands on the model already running — the tier settings name nothing else, or
    // name this very model — is a stay, whatever the tiers say.
    let target = crate::routing::model_for(suggested, &model_low, &model_medium, &model_high);
    let suggested = if target.is_empty() || target == model {
        sim_tier
    } else {
        suggested
    };
    let provider_list = app.providers();
    let near_limit = account_near_limit(&app).await;
    let act = policy(
        sim_tier,
        suggested,
        turns_since_switch,
        context_tokens(tokens),
        providers::pricing_for(&provider_list, &model),
        providers::pricing_for(&provider_list, target).filter(|_| suggested != sim_tier),
        near_limit,
    );
    let record = TurnRecord {
        ts: Utc::now(),
        session: id.clone(),
        turn,
        model,
        current_tier: sim_tier.as_str().to_string(),
        suggested_tier: suggested.as_str().to_string(),
        source,
        verdict: act.verdict,
        would_switch: act.would_switch,
        context_tokens: context_tokens(tokens),
        est_refill_usd: act.est_refill_usd,
        est_saving_usd: act.est_saving_usd,
        account_near_limit: near_limit,
    };
    let line = serde_json::to_string(&record).unwrap_or_default();
    if line.is_empty() {
        return;
    }
    // A lost record is a lost measurement, not a failed turn: the alert is raised and the colony
    // carries on — the same deal every ledger append gets.
    if let Err(e) = app.store().append(&id, FILE, line.as_bytes()).await {
        app.storage_failed("append to the turn-routing ledger", &e.into()).await;
    }
}

/// The per-colony records a bounded tail reaches, oldest first. A long session's earlier turns may
/// be past the tail, which only loosens the hysteresis count's memory — never a switch itself.
async fn record_tail(store: &dyn SessionStore, id: &str) -> Vec<Value> {
    let Ok(Some(bytes)) = store.read_tail(id, FILE, TAIL_BYTES).await else {
        return Vec::new();
    };
    std::str::from_utf8(&bytes)
        .unwrap_or("")
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The turn's own tokens, per model: the cumulative `model_usage` maps differ by what the turn
/// added. The one model that grew most stands for the turn; `None` when nothing grew.
fn turn_delta(old: Option<&Value>, new: Option<&Value>) -> Option<(String, crate::spend::Tokens)> {
    let before: std::collections::BTreeMap<&str, crate::spend::Tokens> = crate::spend::model_tokens(old).into_iter().collect();
    crate::spend::model_tokens(new)
        .into_iter()
        .map(|(model, tokens)| (model, tokens.saturating_sub(before.get(model).copied().unwrap_or_default())))
        .filter(|(_, tokens)| tokens.total() > 0)
        .max_by_key(|(_, tokens)| tokens.total())
        .map(|(model, tokens)| (model.to_string(), tokens))
}

/// The context a turn held, in the issue's terms: its input plus both cache kinds.
fn context_tokens(tokens: crate::spend::Tokens) -> u64 {
    tokens
        .input
        .saturating_add(tokens.cache_read)
        .saturating_add(tokens.cache_write)
}

/// The conversation's current prompt — its last user message — and the entries before it, from the
/// colony's event log. `None` when the log names no prompt, e.g. a tail cut mid-conversation.
fn prompt_and_recent(events: &[Value]) -> Option<(String, impl Iterator<Item = Entry> + '_)> {
    let prompt_at = events
        .iter()
        .rposition(|e| matches!(entry_of(e), Entry::Text(Side::User, _)))?;
    let current = events[prompt_at]["text"].as_str()?.to_string();
    Some((current, events[..prompt_at].iter().map(entry_of)))
}

/// One event line as a conversation entry: the orchestrator's own user and assistant prose counts,
/// everything else — subagent lines, tool traffic, status, deltas — is dropped.
fn entry_of(event: &Value) -> Entry {
    if event.get("agent").is_some() {
        return Entry::Other;
    }
    let text = |event: &Value| event["text"].as_str().filter(|t| !t.trim().is_empty()).map(str::to_string);
    match event["type"].as_str() {
        Some("user_message") => text(event).map_or(Entry::Other, |t| Entry::Text(Side::User, t)),
        Some("assistant_text") => text(event).map_or(Entry::Other, |t| Entry::Text(Side::Assistant, t)),
        _ => Entry::Other,
    }
}

/// The tier the per-turn classification picks for this turn, with what picked it: the rule over the
/// bounded conversation, and — when this colony's Jev mode asks at all — the ask on top, applied the
/// way act mode would apply it. An ask that timed out or failed is a classification that did not
/// finish: the current tier stands and the record says which.
async fn classify(
    bounded: &str,
    current: Tier,
    jev_mode: crate::routing::JevMode,
    org_allows: bool,
    jev_act_confidence: f64,
) -> (Tier, TurnSource) {
    with_timeout(current, async move {
        let mut signals = crate::routing::signals("", bounded, &[], false);
        let ask = crate::jev::shadow_opinion(jev_mode, org_allows, "", &[], &signals).await;
        if let Some(source) = ask.as_ref().and_then(|ask| ask_source(&ask.result)) {
            return (current, source);
        }
        signals.jev = ask.and_then(|ask| ask.opinion);
        // Act is the mode simulated here: the record measures what a switch would pick, so a
        // confident Jev opinion applies exactly as it would in act mode. With no opinion it falls
        // through to the rule, which is the answer either way.
        let decision = crate::routing::decide(
            &crate::routing::RoutingSettings {
                enabled: true,
                chosen: None,
                jev_mode: crate::routing::JevMode::Act,
                jev_act_confidence,
                sensitive: false,
            },
            &signals,
        );
        let source = if decision.source == crate::routing::Source::Jev {
            TurnSource::Jev
        } else {
            TurnSource::Rule
        };
        (decision.tier, source)
    })
    .await
}

/// The one thing about the ask the record names: an ask that timed out or failed ends the
/// classification under `Timeout`/`Error`. Every other miss — the mode off, no key, the org off —
/// is just no opinion, and the rule's own tier stands as `Rule`.
fn ask_source(result: &Result<crate::decide::Decision, crate::decide::Miss>) -> Option<TurnSource> {
    match result {
        Err(crate::decide::Miss::Timeout) => Some(TurnSource::Timeout),
        Err(crate::decide::Miss::Error) => Some(TurnSource::Error),
        _ => None,
    }
}

/// The classification bounded: whatever runs past [`CLASSIFY_TIMEOUT`] — in practice a stalled Jev
/// ask — answers with the current tier under `Timeout`.
async fn with_timeout(current: Tier, inner: impl Future<Output = (Tier, TurnSource)>) -> (Tier, TurnSource) {
    match tokio::time::timeout(CLASSIFY_TIMEOUT, inner).await {
        Ok(answer) => answer,
        Err(_) => (current, TurnSource::Timeout),
    }
}

/// Whether the install's Claude account is close enough to its session cap that stretching the
/// subscription beats the dollar case: the gateway's own exhaustion mark, or a fresh-enough usage
/// reading with the five-hour window at or past [`NEAR_LIMIT_PCT`].
async fn account_near_limit(app: &crate::App) -> bool {
    if app.gateway.is_account_quota_exhausted() {
        return true;
    }
    crate::claude_login::cached_usage(app).await.is_some_and(|reading| {
        reading
            .windows
            .iter()
            .any(|w| w.label == "Session" && w.used_pct >= NEAR_LIMIT_PCT)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::Miss;
    use crate::modules::AgentModule;
    use crate::sessions::SessionStatus;
    use serde_json::json;

    fn price(input: f64, read: f64, write: f64) -> Pricing {
        Pricing {
            input_per_mtok: input,
            output_per_mtok: 5.0,
            cache_read_per_mtok: read,
            cache_write_per_mtok: write,
            thinking_per_mtok: 0.0,
        }
    }

    // The arithmetic the tests below reason about, per million tokens: the cheap model refills the
    // context once at 1.50 and reads it back for 0.10, the dear model at `read`, so a downgrade
    // over the three-turn window saves 3 × (read − 0.10) against that one-time 1.50 — 0.60 at a
    // 0.30 read (not worth it), 2.40 at a 0.90 read (worth it). Both sides scale with the context,
    // so the window is the amortisation: what flips a downgrade is the rate gap, not the size.
    fn dear(read: f64) -> Pricing {
        price(3.0, read, 3.75)
    }

    fn cheap() -> Pricing {
        price(1.5, 0.10, 1.5)
    }

    // -- bounded input --------------------------------------------------------------------------

    #[test]
    fn bounded_input_keeps_the_last_six_texts_oldest_first_current_last() {
        let recent = (0..8).map(|i| Entry::Text(if i % 2 == 0 { Side::User } else { Side::Assistant }, format!("note {i}")));
        let input = bounded_input("do it", recent);
        assert!(
            !input.contains("note 0") && !input.contains("note 1"),
            "the window is the most recent: {input}"
        );
        assert!(input.starts_with("user: note 2"), "oldest kept first: {input}");
        assert!(input.contains("assistant: note 7"));
        assert!(input.ends_with("\nuser: do it"), "the current prompt is last: {input}");
        assert_eq!(input.lines().count(), RECENT_TEXTS + 1);
    }

    #[test]
    fn bounded_input_drops_everything_that_is_not_user_or_assistant_text() {
        let recent = [Entry::Other, Entry::Text(Side::User, "hi".into()), Entry::Other].into_iter();
        assert_eq!(bounded_input("go", recent), "user: hi\nuser: go");
    }

    #[test]
    fn bounded_input_caps_each_text_on_a_char_boundary() {
        let multibyte = "é".repeat(TEXT_CAP + 50);
        let input = bounded_input(&multibyte, std::iter::empty());
        assert_eq!(
            input.chars().count(),
            "user: ".len() + TEXT_CAP,
            "cut to the cap, never mid-character"
        );
        let ascii = "x".repeat(TEXT_CAP + 50);
        assert_eq!(bounded_input(&ascii, std::iter::empty()).len(), "user: ".len() + TEXT_CAP);
    }

    // -- timeout ---------------------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_classification_that_never_finishes_keeps_the_current_tier() {
        let (tier, source) = with_timeout(Tier::Medium, std::future::pending()).await;
        assert_eq!(tier, Tier::Medium, "the current tier stands");
        assert_eq!(source, TurnSource::Timeout, "and the record names why");
    }

    #[test]
    fn a_missed_ask_names_timeout_or_error_and_any_other_miss_leaves_the_rule() {
        assert_eq!(ask_source(&Err(Miss::Timeout)), Some(TurnSource::Timeout));
        assert_eq!(ask_source(&Err(Miss::Error)), Some(TurnSource::Error));
        assert_eq!(ask_source(&Err(Miss::NoKey)), None, "no key is just no opinion");
        assert_eq!(ask_source(&Err(Miss::OrgOff)), None);
    }

    // -- the act-mode policy, evaluated in shadow -------------------------------------------------

    #[test]
    fn a_downgrade_switches_only_when_the_saving_beats_the_refill() {
        let act = policy(
            Tier::High,
            Tier::Low,
            HYSTERESIS_TURNS,
            1_000_000,
            Some(dear(0.30)),
            Some(cheap()),
            false,
        );
        assert_eq!(
            act.verdict,
            Verdict::CacheRefillExceedsSaving,
            "$0.60 of saving does not cover a $1.50 refill"
        );
        assert_eq!(act.est_refill_usd, Some(1.5));
        assert!(
            (act.est_saving_usd.unwrap() - 0.6).abs() < 1e-9,
            "got {:?}",
            act.est_saving_usd
        );
        assert!(!act.would_switch);
        let act = policy(
            Tier::High,
            Tier::Low,
            HYSTERESIS_TURNS,
            1_000_000,
            Some(dear(0.90)),
            Some(cheap()),
            false,
        );
        assert_eq!(
            act.verdict,
            Verdict::Switch,
            "a dearer cache read turns the same window into a saving"
        );
        assert_eq!(act.est_refill_usd, Some(1.5));
        assert!(
            (act.est_saving_usd.unwrap() - 2.4).abs() < 1e-9,
            "got {:?}",
            act.est_saving_usd
        );
        assert!(act.would_switch);
    }

    #[test]
    fn hysteresis_holds_a_downgrade_until_the_window_has_passed() {
        for turns in 0..HYSTERESIS_TURNS {
            let act = policy(
                Tier::High,
                Tier::Low,
                turns,
                1_000_000,
                Some(dear(0.90)),
                Some(cheap()),
                false,
            );
            assert_eq!(
                act.verdict,
                Verdict::HeldByHysteresis,
                "{turns} turn(s) after a switch is still inside the window"
            );
            assert!(!act.would_switch);
        }
        let act = policy(
            Tier::High,
            Tier::Low,
            HYSTERESIS_TURNS,
            1_000_000,
            Some(dear(0.90)),
            Some(cheap()),
            false,
        );
        assert_eq!(
            act.verdict,
            Verdict::Switch,
            "the window has passed, so the dollar case decides"
        );
    }

    #[test]
    fn an_upgrade_is_never_second_guessed_even_unpriced() {
        let act = policy(Tier::Low, Tier::High, 0, 1_000_000, None, None, false);
        assert_eq!(act.verdict, Verdict::Switch);
        assert_eq!(
            act.est_refill_usd, None,
            "an unpriced upgrade still switches, with no figures invented"
        );
        assert_eq!(act.est_saving_usd, None);
    }

    #[test]
    fn an_unpriced_downgrade_is_held_unless_the_account_is_near_its_limit() {
        let act = policy(Tier::High, Tier::Low, HYSTERESIS_TURNS, 1_000_000, None, Some(cheap()), false);
        assert_eq!(act.verdict, Verdict::Unpriced);
        assert!(!act.would_switch);
        let act = policy(Tier::High, Tier::Low, HYSTERESIS_TURNS, 1_000_000, None, Some(cheap()), true);
        assert_eq!(
            act.verdict,
            Verdict::Switch,
            "near the session cap the downgrade does not need the dollar case"
        );
        assert!(act.would_switch);
    }

    #[test]
    fn the_same_tier_is_a_stay_with_no_figures() {
        let act = policy(
            Tier::Medium,
            Tier::Medium,
            0,
            1_000_000,
            Some(dear(0.30)),
            Some(cheap()),
            false,
        );
        assert_eq!(act.verdict, Verdict::Stay);
        assert_eq!(act.est_refill_usd, None);
        assert!(!act.would_switch);
    }

    // -- reading the turn back --------------------------------------------------------------------

    #[test]
    fn the_turn_is_the_model_that_grew_most_and_none_when_nothing_grew() {
        let old = json!({"m-a": {"input_tokens": 100, "output_tokens": 10}, "m-b": {"input_tokens": 5}});
        let new =
            json!({"m-a": {"input_tokens": 160, "output_tokens": 10}, "m-b": {"input_tokens": 500, "cache_read_tokens": 20}});
        let (model, tokens) = turn_delta(Some(&old), Some(&new)).unwrap();
        assert_eq!(model, "m-b", "the model whose cumulative grew most stands for the turn");
        assert_eq!(tokens.input, 495, "the delta, not the cumulative");
        assert_eq!(tokens.cache_read, 20);
        assert!(
            turn_delta(Some(&old), Some(&old)).is_none(),
            "a turn that grew nothing has no model to route on"
        );
        assert!(turn_delta(None, None).is_none());
    }

    #[test]
    fn the_context_is_the_turns_input_plus_both_cache_kinds() {
        let tokens = crate::spend::Tokens {
            input: 10,
            output: 4,
            cache_read: 300,
            cache_write: 6,
        };
        assert_eq!(context_tokens(tokens), 316, "output is not context");
    }

    #[test]
    fn the_prompt_is_the_last_lead_user_message_and_subagent_lines_drop_out() {
        let events = vec![
            json!({"type": "user_message", "text": "first"}),
            json!({"type": "assistant_text", "text": "working", "agent": "scout"}),
            json!({"type": "user_message", "text": "  ", "agent": "scout"}),
            json!({"type": "tool_call", "name": "bash", "input": "ls"}),
            json!({"type": "assistant_text", "text": "halfway there"}),
            json!({"type": "user_message", "text": "now the fix"}),
        ];
        let (current, recent) = prompt_and_recent(&events).unwrap();
        assert_eq!(current, "now the fix");
        let recent: Vec<_> = recent.collect();
        assert_eq!(
            recent,
            vec![
                Entry::Text(Side::User, "first".into()),
                Entry::Other,
                Entry::Other,
                Entry::Other,
                Entry::Text(Side::Assistant, "halfway there".into()),
            ],
            "the subagent's lines and the tool call are not the lead's conversation"
        );
        assert!(
            prompt_and_recent(&[json!({"type": "tool_call"})]).is_none(),
            "no prompt, no classification"
        );
    }

    // -- the entry point, over a real App ----------------------------------------------------------

    async fn app_with_agent(schema: Value) -> (crate::Shared, std::path::PathBuf) {
        let root = crate::tests::temp_root();
        let mut agent = AgentModule::test("claude-code");
        agent.schema = schema;
        let app = crate::tests::test_app_with_agents(&root, vec![agent], |_| {});
        let mut s = crate::sessions::tests::colony("acme", SessionStatus::Running);
        s.id = "abc".to_string();
        s.agent = "claude-code".to_string();
        app.sessions.write().await.push(s);
        tokio::fs::create_dir_all(app.session_dir("abc")).await.unwrap();
        (app, root)
    }

    fn usage(model: &str) -> Value {
        json!({model: {"input_tokens": 100, "output_tokens": 20, "cache_read_tokens": 500, "cache_write_tokens": 0}})
    }

    async fn records(app: &crate::Shared) -> Vec<Value> {
        let path = app.session_dir("abc").join(FILE);
        match tokio::fs::read_to_string(path).await {
            Ok(text) => text.lines().map(|l| serde_json::from_str(l).unwrap()).collect(),
            Err(_) => Vec::new(),
        }
    }

    #[tokio::test]
    async fn an_orchestrator_turn_end_appends_one_record() {
        let (app, root) = app_with_agent(json!({})).await;
        app.store()
            .append(
                "abc",
                "events.jsonl",
                br#"{"type":"user_message","text":"Fix the flaky test"}"#,
            )
            .await
            .unwrap();
        app.update_session("abc", |s| s.model_routing = Some(json!({"tier": "high"})))
            .await;

        on_turn_end(app.clone(), "abc".into(), false, None, Some(usage("claude-sonnet-4"))).await;

        let records = records(&app).await;
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r["session"], "abc");
        assert_eq!(r["turn"], 0, "the first recorded turn end is turn zero");
        assert_eq!(r["model"], "claude-sonnet-4", "the model that ran the turn");
        assert_eq!(r["current_tier"], "high", "seeded by the boot routing record");
        assert_eq!(
            r["suggested_tier"], "high",
            "the tier settings name no other model, so a suggestion is a stay"
        );
        assert_eq!(r["source"], "rule", "the mode is off, so the rule decided");
        assert_eq!(r["verdict"], "stay");
        assert_eq!(r["would_switch"], false);
        assert_eq!(r["context_tokens"], 600);
        assert_eq!(
            r["est_refill_usd"],
            Value::Null,
            "no provider pricing configured, so no figures invented"
        );
        assert_eq!(r["est_saving_usd"], Value::Null);
        assert_eq!(r["account_near_limit"], false);
        assert_eq!(
            r["ts"].as_str().map(|t| t.len()),
            Some(30),
            "an RFC 3339 stamp with nanoseconds"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_subagents_turn_end_is_never_routed() {
        let (app, root) = app_with_agent(json!({})).await;
        app.store()
            .append(
                "abc",
                "events.jsonl",
                br#"{"type":"user_message","text":"Fix the flaky test"}"#,
            )
            .await
            .unwrap();
        on_turn_end(app.clone(), "abc".into(), true, None, Some(usage("claude-sonnet-4"))).await;
        assert!(
            records(&app).await.is_empty(),
            "a subagent's turn is its own; the shadow never routes it"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_setting_off_records_nothing() {
        let schema = json!({"properties": {"turn_route_shadow": {"type": "boolean", "default": false}}});
        let (app, root) = app_with_agent(schema).await;
        app.store()
            .append(
                "abc",
                "events.jsonl",
                br#"{"type":"user_message","text":"Fix the flaky test"}"#,
            )
            .await
            .unwrap();
        on_turn_end(app.clone(), "abc".into(), false, None, Some(usage("claude-sonnet-4"))).await;
        assert!(records(&app).await.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_turn_with_no_usage_records_nothing_even_with_a_prompt() {
        let (app, root) = app_with_agent(json!({})).await;
        app.store()
            .append(
                "abc",
                "events.jsonl",
                br#"{"type":"user_message","text":"Fix the flaky test"}"#,
            )
            .await
            .unwrap();
        on_turn_end(app.clone(), "abc".into(), false, None, None).await;
        assert!(records(&app).await.is_empty(), "no delta, no record");
        let _ = std::fs::remove_dir_all(root);
    }
}
