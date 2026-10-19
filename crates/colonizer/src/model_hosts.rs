//! The cheapest-host view of the model fleet (issue #1039): one canonical model often served by
//! several connected providers at different prices, each host ranked by blended price
//! (`GET /api/model-hosts`), and the opt-in boot policy that re-pins a colony's `COLONIZER_*_MODEL`
//! settings onto the cheapest healthy host serving the same canonical.
//!
//! The ranking and the policy are pure ([`host_ranking`], [`reroute_model`]): providers, the price
//! feed's entries and a per-provider health/quota state go in, an answer comes out — so both are
//! tested without an `App`. [`rankings`] is the thin handler wrapper, and [`host_state`] is the one
//! place the gateway's health and quota reads are composed (the same reads the model picker shows,
//! `model_switch::model_options`).

use crate::{
    App,
    gateway::health,
    price_feed::{FeedEntries, FeedEntry},
    providers::{Pricing, Provider, provider_prefix},
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// One host of a canonical model: a configured provider that serves it, priced, with the health,
/// quota and policy flags the ranking and the cockpit both read. All rates are dollars per million
/// tokens; every price field is `null` when nothing prices the model on this host — the pricing
/// precedence ([`Provider::price_for`]) is per-model price → provider price → the price feed, so a
/// feed that is down falls back to the operator's own prices rather than to "unpriced".
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ModelHost {
    pub provider_id: String,
    pub provider_name: String,
    /// The model id a colony's setting would carry on this host: `<provider_id>/<canonical>`.
    pub model: String,
    pub input_per_mtok: Option<f64>,
    pub output_per_mtok: Option<f64>,
    pub cache_read_per_mtok: Option<f64>,
    /// [`blended_per_mtok`] of the host's price, the rate the ranking sorts by.
    pub blended_per_mtok: Option<f64>,
    /// When the price came from the feed (price_feed.rs): the moment the feed last vouched for it,
    /// and where the feed says the price came from. `null` for an operator-set price — the
    /// operator is their own provenance — and for no price at all.
    pub last_verified_at: Option<String>,
    pub source: Option<String>,
    /// Not degraded and the plan not out: a host the policy may pin a colony onto.
    pub healthy: bool,
    /// `gateway::health` degraded, or the plan out — the same composition the model picker shows.
    pub degraded: bool,
    pub out_of_quota: bool,
    /// When the exhausted plan refills, while it is out; `null` otherwise.
    pub quota_reset_at: Option<String>,
    /// The provider strips Claude Code tools from every request: a colony pinned here would run
    /// without them, so the policy skips it.
    pub tools_limited: bool,
    /// This provider lists the canonical in its `host_sensitive_models`: quantisation or context
    /// limits differ per host, so the model must stay where the operator put it.
    pub host_sensitive: bool,
}

/// One canonical model's hosts in ranked order, as `GET /api/model-hosts` lists it. `eligible`
/// says whether the boot policy may auto-reroute the canonical: it looks open-weight AND no
/// provider marks it host-sensitive.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ModelHostRanking {
    /// The canonical model id — the part after `<provider>/` in a setting.
    pub model: String,
    pub eligible: bool,
    pub hosts: Vec<ModelHost>,
}

/// A provider's health and quota state, as the ranking and the policy need it, composed by
/// [`host_state`] — the same reads `model_switch::model_options` shows the picker, so a host the
/// picker would grey out is a host the policy skips.
pub(crate) struct HostState {
    /// `gateway::health` over the provider's usage says degraded (a rated provider failing too
    /// often). Quota exhaustion is reported separately and folds into the host's `degraded`.
    pub degraded: bool,
    pub out_of_quota: bool,
    /// The plan's reset, when one is recorded and the plan is out.
    pub quota_reset_at: Option<String>,
    /// The connection is usable: `auth: "none"`, or an API key is actually saved
    /// (`chat.rs`'s `has_key`). A keyless host would 502 on every call, so it is neither healthy
    /// nor choosable — the same fold the model picker shows.
    pub connected: bool,
}

/// The blended $/1M-token rate the ranking sorts by: three parts input to one part output, roughly
/// a coding agent's shape — the prompt, files and re-reads dominate, the answer is the minority.
/// When a cache-read rate is known, one of the three input parts is priced at it (cache reads are
/// typically far cheaper than fresh input), encoding "cache-aware when known":
/// `(2·input + cache_read + output) / 4`; otherwise `(3·input + output) / 4`.
///
/// Whether the cache-aware form is used must be decided once per ranking, not per host, so the
/// rate is comparable across hosts: a host that publishes a cache-read rate must not earn a
/// discount a host that does not publish one can never. [`host_ranking`] computes that once —
/// `cache_known` is true only when every priced host in the ranking publishes a cache-read rate —
/// and passes it here, so the two forms are never mixed within one ranking.
fn blended_per_mtok(pricing: &Pricing, cache_known: bool) -> f64 {
    if cache_known && pricing.cache_read_per_mtok > 0.0 {
        (2.0 * pricing.input_per_mtok + pricing.cache_read_per_mtok + pricing.output_per_mtok) / 4.0
    } else {
        (3.0 * pricing.input_per_mtok + pricing.output_per_mtok) / 4.0
    }
}

/// Whether a canonical counts as open-weight, and so may move between hosts: the closed labs are
/// exclusive first-party hosts with no second host to choose, so a canonical naming one is left
/// alone. Token-aware rather than a bare prefix match, because canonicals do not always carry the
/// lab's prefix — a bare alias (`opus`, `sonnet`) and a Bedrock-style id
/// (`us.anthropic.claude-opus-5-5`) close just as much as `claude-opus-5`, and a closed family
/// served through an aggregator (`grok-4`) still names the lab. Tokens split on any
/// non-alphanumeric, so a dotted or hyphenated id is one closed token away from grounded.
/// `gpt-oss` is the open-weight exception to the closed `gpt` token: OpenAI's own open release,
/// matched as a leading `gpt-oss` so it is not blocked by its family name. What slips through,
/// the operator's `host_sensitive_models` list catches.
fn is_open_weight(canonical: &str) -> bool {
    /// Closed-lab tokens a canonical may spell as a whole segment: the family name and Anthropic's
    /// bare aliases. Matched per segment, not as a substring, so an open-weight family that merely
    /// shares a prefix (`gpt-oss`) is not blocked by it.
    const CLOSED_TOKENS: &[&str] = &[
        "claude", "gpt", "gemini", "grok", "opus", "sonnet", "haiku", "fable", "o1", "o3", "o4",
    ];
    let lower = canonical.to_ascii_lowercase();
    if lower.starts_with("gpt-oss") {
        return true;
    }
    !lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| CLOSED_TOKENS.contains(&token))
}

/// Whether any provider lists the canonical in its `host_sensitive_models` (exact match): one
/// host whose copy of the model differs per host is enough to ground the canonical — the operator
/// named a model, not a host, so no host may be chosen for them.
fn canonical_is_host_sensitive(providers: &[Provider], canonical: &str) -> bool {
    providers
        .iter()
        .any(|p| p.host_sensitive_models.iter().any(|m| m == canonical))
}

/// The price a host charges for the canonical, with the feed entry the price came from when the
/// feed supplied it. [`Provider::price_for`]'s precedence computed explicitly — per-model price,
/// then the provider-level price, then the feed — so the ranking can attach the feed's
/// verification (`last_verified_at`, `source`) to a feed-priced host and to nothing else.
fn priced<'a>(provider: &Provider, canonical: &str, feed: Option<&'a FeedEntries>) -> Option<(Pricing, Option<&'a FeedEntry>)> {
    if let Some(pricing) = provider.model_pricing.get(canonical) {
        return Some((*pricing, None));
    }
    if let Some(pricing) = provider.pricing {
        return Some((pricing, None));
    }
    let entry = feed?.get(provider.feed_provider_id())?.get(canonical)?;
    Some((entry.pricing, Some(entry)))
}

/// Whether the provider serves the canonical: it lists the model, and — when its `model_map` is
/// authoritative — the map names it. The boot's own `unusable_route` rule: an empty map means
/// anything goes, a non-empty one is exactly what the connection serves.
fn serves(provider: &Provider, canonical: &str) -> bool {
    provider.models.iter().any(|m| m == canonical)
        && (provider.model_map.is_empty() || provider.model_map.contains_key(canonical))
}

/// Priced hosts first, cheapest blended rate first, unpriced hosts last.
fn by_blended(a: &ModelHost, b: &ModelHost) -> Ordering {
    match (&a.blended_per_mtok, &b.blended_per_mtok) {
        (Some(a), Some(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// [`by_blended`] with the tie broken by provider id — the order the ranking is served in.
fn by_blended_then_id(a: &ModelHost, b: &ModelHost) -> Ordering {
    by_blended(a, b).then_with(|| a.provider_id.cmp(&b.provider_id))
}

/// One canonical's hosts, ranked — or `None` when fewer than two candidates serve it, because a
/// single-host model has no cheapest host to choose. Every configured provider that serves the
/// canonical contributes a host, priced by [`priced`] and stamped with the health/quota state
/// `state` reports for it.
fn host_ranking(
    providers: &[Provider],
    feed: Option<&FeedEntries>,
    canonical: &str,
    state: &impl Fn(&Provider) -> HostState,
) -> Option<ModelHostRanking> {
    let entries: Vec<(&Provider, HostState, Option<(Pricing, Option<&FeedEntry>)>)> = providers
        .iter()
        .filter(|p| serves(p, canonical))
        .map(|provider| {
            let s = state(provider);
            (provider, s, priced(provider, canonical, feed))
        })
        .collect();
    if entries.len() < 2 {
        return None;
    }
    // Comparable across hosts: the cache-aware blend only when every priced host in this ranking
    // publishes a cache-read rate, so a host that publishes one does not earn a discount a host
    // that does not can never. An unpriced host has no rate to publish, so it does not count
    // against the comparison.
    let cache_known = entries
        .iter()
        .all(|(_, _, found)| found.as_ref().is_none_or(|(p, _)| p.cache_read_per_mtok > 0.0));
    let mut hosts: Vec<ModelHost> = entries
        .into_iter()
        .map(|(provider, s, found)| {
            let pricing = found.as_ref().map(|(p, _)| *p);
            let from_feed = found.and_then(|(_, f)| f);
            ModelHost {
                provider_id: provider.id.clone(),
                provider_name: provider.name.clone(),
                model: format!("{}/{}", provider.id, canonical),
                input_per_mtok: pricing.map(|p| p.input_per_mtok),
                output_per_mtok: pricing.map(|p| p.output_per_mtok),
                cache_read_per_mtok: pricing.map(|p| p.cache_read_per_mtok),
                blended_per_mtok: pricing.as_ref().map(|p| blended_per_mtok(p, cache_known)),
                last_verified_at: from_feed.and_then(|e| e.last_verified_at.clone()),
                source: from_feed.and_then(|e| e.source.clone()),
                // The picker's composition: quota exhaustion counts as degraded, and the reset is
                // shown only while the plan is out (the state closure already gates it).
                degraded: s.degraded || s.out_of_quota,
                healthy: s.connected && !s.degraded && !s.out_of_quota,
                out_of_quota: s.out_of_quota,
                quota_reset_at: s.quota_reset_at,
                tools_limited: !provider.disabled_tools.is_empty(),
                host_sensitive: provider.host_sensitive_models.iter().any(|m| m == canonical),
            }
        })
        .collect();
    hosts.sort_by(by_blended_then_id);
    Some(ModelHostRanking {
        model: canonical.to_string(),
        eligible: is_open_weight(canonical) && !canonical_is_host_sensitive(providers, canonical),
        hosts,
    })
}

/// The boot-time policy core (issue #1039): what a `<provider>/<model>` setting becomes on the
/// cheapest healthy host serving the same canonical, or `None` to keep it as it is. Only
/// open-weight models, never one any provider marks host-sensitive, never a canonical fewer than
/// two candidates serve or none of them prices (nothing to rank on), and never a setting whose own
/// host is not among the candidates — the operator listed the hosts they want, and data they did
/// not list is not touched. A host is choosable when it is priced, healthy (not degraded, plan not
/// out) and strips no tools; ties keep the current host. The canonical itself never changes — only
/// the host serving it may.
///
/// `eligible` says whether a candidate provider may carry this colony's work for its sensitivity
/// class — the gateway's own rule (`providers::provider_eligible`), passed in so the policy cannot
/// move a colony onto a host the gateway would then 403. The ranking path (`GET /api/model-hosts`,
/// no session) passes a permissive predicate.
pub(crate) fn reroute_model(
    providers: &[Provider],
    feed: Option<&FeedEntries>,
    current: &str,
    state: &impl Fn(&Provider) -> HostState,
    eligible: &dyn Fn(&Provider) -> bool,
) -> Option<String> {
    let prefix = provider_prefix(current)?;
    let canonical = &current[prefix.len() + 1..];
    if !is_open_weight(canonical) || canonical_is_host_sensitive(providers, canonical) {
        return None;
    }
    let ranking = host_ranking(providers, feed, canonical, state)?;
    if !ranking.hosts.iter().any(|h| h.provider_id == prefix) {
        return None;
    }
    if !ranking.hosts.iter().any(|h| h.blended_per_mtok.is_some()) {
        return None;
    }
    let mut choosable: Vec<&ModelHost> = ranking
        .hosts
        .iter()
        .filter(|h| {
            h.blended_per_mtok.is_some()
                && h.healthy
                && !h.tools_limited
                && !h.host_sensitive
                && providers.iter().any(|p| p.id == h.provider_id && eligible(p))
        })
        .collect();
    // Cheapest first, and on a blended-price tie the host the setting already names — a rewrite
    // needs a reason, and an equal price is not one — before the ranking's own provider-id order
    // would settle the tie alphabetically and move the colony for nothing.
    choosable.sort_by(|a, b| {
        by_blended(a, b)
            .then_with(|| match (a.provider_id == prefix, b.provider_id == prefix) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => Ordering::Equal,
            })
            .then_with(|| a.provider_id.cmp(&b.provider_id))
    });
    let best = choosable.first()?;
    (best.provider_id != prefix).then(|| format!("{}/{}", best.provider_id, canonical))
}

/// A provider's [`HostState`], composed exactly as `model_switch::model_options` composes the
/// picker's rows: `gateway::health` over the provider's usage, the plan's exhaustion, the reset
/// shown only while the plan is out, and the connection's being usable — `auth: "none"` or a key
/// actually saved (`chat.rs`'s `has_key`), so a keyless host is neither healthy nor choosable.
pub(crate) fn host_state(app: &App, provider: &Provider) -> HostState {
    let h = health(&app.gateway.usage(&provider.id));
    let out_of_quota = app.gateway.is_quota_exhausted(&provider.id);
    HostState {
        degraded: h.degraded,
        out_of_quota,
        quota_reset_at: out_of_quota
            .then(|| app.gateway.quota_state(&provider.id))
            .flatten()
            .and_then(|q| q.reset_at),
        connected: provider.auth == "none" || app.provider_key(&provider.id).is_some(),
    }
}

/// `GET /api/model-hosts`: every canonical two or more configured providers serve, its hosts
/// ranked by blended price — the cheapest-host view of the fleet (issue #1039). Canonicals only
/// one provider serves have no ranking and are not listed.
pub(crate) fn rankings(app: &App) -> Vec<ModelHostRanking> {
    let providers = app.providers();
    let feed = app.price_feed.entries();
    let state = |p: &Provider| host_state(app, p);
    let mut canonicals: Vec<String> = providers.iter().flat_map(|p| p.models.iter().cloned()).collect();
    canonicals.sort();
    canonicals.dedup();
    canonicals
        .into_iter()
        .filter_map(|canonical| host_ranking(&providers, Some(feed.as_ref()), &canonical, &state))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    fn pricing(input: f64, output: f64, cache_read: f64) -> Pricing {
        Pricing {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: cache_read,
            ..Default::default()
        }
    }

    /// A provider built from JSON, so every field the test does not name keeps its serde default —
    /// the shape a providers.json row has.
    fn provider(id: &str, models: &[&str]) -> Provider {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "base_url": "http://127.0.0.1:9", "auth": "none",
            "models": models,
        }))
        .unwrap()
    }

    fn healthy() -> HostState {
        HostState {
            degraded: false,
            out_of_quota: false,
            quota_reset_at: None,
            connected: true,
        }
    }

    fn degraded() -> HostState {
        HostState {
            degraded: true,
            out_of_quota: false,
            quota_reset_at: None,
            connected: true,
        }
    }

    fn out_of_quota() -> HostState {
        HostState {
            degraded: false,
            out_of_quota: true,
            quota_reset_at: Some("Oct 6, 09:00 UTC".into()),
            connected: true,
        }
    }

    /// Every provider healthy, the plain case.
    fn all_healthy(_: &Provider) -> HostState {
        healthy()
    }

    /// A permissive eligibility predicate: every provider carries this class. The ranking path
    /// passes one, and these unit tests hold no sensitivity class.
    fn permissive(_: &Provider) -> bool {
        true
    }

    #[test]
    fn blended_price_is_three_parts_input_to_one_output_cache_aware_when_known() {
        // No cache-read rate (or cache not known for the ranking): all three input parts pay it.
        assert!(close(blended_per_mtok(&pricing(1.0, 2.0, 0.0), false), 1.25));
        assert!(close(blended_per_mtok(&pricing(1.0, 2.0, 0.5), false), 1.25));
        // With cache known: one of the three input parts pays the cache-read rate instead.
        assert!(close(blended_per_mtok(&pricing(1.0, 2.0, 0.1), true), 1.025));
        // A cache read cheaper than input lowers the blend; one dearer would raise it.
        assert!(blended_per_mtok(&pricing(1.0, 2.0, 0.5), true) < blended_per_mtok(&pricing(1.0, 2.0, 0.0), true));
    }

    #[test]
    fn the_closed_lab_denylist_decides_what_counts_as_open_weight() {
        for closed in [
            "claude-x",
            "gpt-5",
            "o1-preview",
            "o3-mini",
            "o4-mini",
            "gemini-3",
            "grok-4",
            "GPT-5-Turbo",
            // Bare aliases and Bedrock-style ids: a closed lab is closed whatever it is spelled as.
            "opus",
            "sonnet",
            "haiku",
            "fable",
            "us.anthropic.claude-opus-5-5",
        ] {
            assert!(!is_open_weight(closed), "{closed} is a closed lab's model");
        }
        for open in [
            "deepseek-v4-flash",
            "glm-5.3",
            "kimi-k2",
            "qwen3.8-max",
            "minimax-m3.1",
            // `gpt-oss` is OpenAI's open-weight release: the closed `gpt` token does not close it.
            "gpt-oss-120b",
            "gpt-oss-20b",
        ] {
            assert!(is_open_weight(open), "{open} counts as open-weight");
        }
    }

    #[test]
    fn hosts_rank_cheapest_first_unpriced_last_ties_by_provider_id() {
        let mut pricey = provider("zai", &["glm-5"]);
        pricey.pricing = Some(pricing(1.0, 2.0, 0.0));
        let mut cheap = provider("deepinfra", &["glm-5"]);
        cheap.pricing = Some(pricing(0.2, 0.8, 0.0));
        let unpriced = provider("together", &["glm-5"]);
        let ranking = host_ranking(&[pricey, cheap.clone(), unpriced], None, "glm-5", &all_healthy).unwrap();
        assert_eq!(
            ranking.hosts.iter().map(|h| h.provider_id.as_str()).collect::<Vec<_>>(),
            ["deepinfra", "zai", "together"],
            "priced hosts by blended price, the unpriced one last"
        );
        assert_eq!(
            ranking.hosts[0].blended_per_mtok,
            Some(blended_per_mtok(&pricing(0.2, 0.8, 0.0), false)),
            "cheap's (3·0.2 + 0.8) / 4"
        );
        assert_eq!(ranking.hosts[2].blended_per_mtok, None, "no price, no blend");
        assert_eq!(ranking.hosts[2].input_per_mtok, None);
        assert!(ranking.eligible, "open-weight and unmarked");

        // A tie keeps provider-id order.
        let mut also_cheap = provider("aionero", &["glm-5"]);
        also_cheap.pricing = Some(pricing(0.2, 0.8, 0.0));
        let tied = host_ranking(&[cheap, also_cheap], None, "glm-5", &all_healthy).unwrap();
        assert_eq!(
            tied.hosts.iter().map(|h| h.provider_id.as_str()).collect::<Vec<_>>(),
            ["aionero", "deepinfra"]
        );
    }

    #[test]
    fn a_degraded_or_quotaless_host_is_flagged_like_the_picker_flags_it() {
        let mut a = provider("a", &["glm-5"]);
        a.pricing = Some(pricing(1.0, 1.0, 0.0));
        let mut b = provider("b", &["glm-5"]);
        b.pricing = Some(pricing(1.0, 1.0, 0.0));
        let state = |p: &Provider| match p.id.as_str() {
            "a" => degraded(),
            "b" => out_of_quota(),
            _ => healthy(),
        };
        let ranking = host_ranking(&[a, b], None, "glm-5", &state).unwrap();
        let a = &ranking.hosts[0];
        assert!((a.degraded, a.healthy, a.out_of_quota, a.quota_reset_at.as_deref()) == (true, false, false, None));
        let b = &ranking.hosts[1];
        assert!(
            (b.degraded, b.healthy, b.out_of_quota, b.quota_reset_at.as_deref()) == (true, false, true, Some("Oct 6, 09:00 UTC")),
            "quota exhaustion counts as degraded, and carries the reset"
        );
    }

    #[test]
    fn reroute_picks_the_cheapest_healthy_host_and_skips_the_marked_ones() {
        let mut current = provider("zai", &["glm-5"]);
        current.pricing = Some(pricing(1.0, 2.0, 0.0));
        let mut cheap_degraded = provider("cheap-degraded", &["glm-5"]);
        cheap_degraded.pricing = Some(pricing(0.1, 0.1, 0.0));
        let mut cheap_quota = provider("cheap-quota", &["glm-5"]);
        cheap_quota.pricing = Some(pricing(0.15, 0.15, 0.0));
        let mut cheap_tools = provider("cheap-tools", &["glm-5"]);
        cheap_tools.pricing = Some(pricing(0.2, 0.2, 0.0));
        cheap_tools.disabled_tools = vec!["Bash".into()];
        let mut next = provider("next", &["glm-5"]);
        next.pricing = Some(pricing(0.5, 0.5, 0.0));
        let providers = [current, cheap_degraded, cheap_quota, cheap_tools, next.clone()];
        let state = |p: &Provider| match p.id.as_str() {
            "cheap-degraded" => degraded(),
            "cheap-quota" => out_of_quota(),
            _ => healthy(),
        };
        assert_eq!(
            reroute_model(&providers, None, "zai/glm-5", &state, &permissive),
            Some("next/glm-5".into()),
            "cheaper-but-degraded, cheaper-but-out and cheaper-but-tools-limited are all skipped"
        );
        // However cheap a host is, its marking the canonical host-sensitive grounds the canonical:
        // the operator said this model's host is not interchangeable, so nothing moves.
        let mut marked = next.clone();
        marked.id = "cheap-marked".into();
        marked.pricing = Some(pricing(0.05, 0.05, 0.0));
        marked.host_sensitive_models = vec!["glm-5".into()];
        let providers = [provider("zai", &["glm-5"]), next.clone(), marked];
        assert_eq!(
            reroute_model(&providers, None, "zai/glm-5", &|_| healthy(), &permissive),
            None,
            "one host-sensitive mark grounds the canonical, however cheap the marking host"
        );
    }

    #[test]
    fn no_rewrite_when_the_current_host_is_already_cheapest_or_wins_the_tie() {
        let mut current = provider("zai", &["glm-5"]);
        current.pricing = Some(pricing(0.2, 0.2, 0.0));
        let mut dearer = provider("dear", &["glm-5"]);
        dearer.pricing = Some(pricing(1.0, 1.0, 0.0));
        assert_eq!(
            reroute_model(&[current.clone(), dearer], None, "zai/glm-5", &all_healthy, &permissive),
            None
        );

        // A tie with the current host keeps the current host.
        let mut tied = provider("aaa-tied", &["glm-5"]);
        tied.pricing = Some(pricing(0.2, 0.2, 0.0));
        assert_eq!(
            reroute_model(&[current.clone(), tied.clone()], None, "zai/glm-5", &all_healthy, &permissive),
            None,
            "an equal price is not a reason to move"
        );
        assert_eq!(
            reroute_model(&[current.clone(), tied], None, "aaa-tied/glm-5", &all_healthy, &permissive),
            None,
            "and a tie never moves anyone"
        );
    }

    #[test]
    fn a_closed_model_never_rewrites_even_when_two_providers_serve_it() {
        let mut a = provider("a", &["claude-sonnet-4-5"]);
        a.pricing = Some(pricing(1.0, 1.0, 0.0));
        let mut b = provider("b", &["claude-sonnet-4-5"]);
        b.pricing = Some(pricing(0.1, 0.1, 0.0));
        assert_eq!(
            reroute_model(
                &[a, b.clone()],
                None,
                "anthropic/claude-sonnet-4-5",
                &all_healthy,
                &permissive
            ),
            None,
            "the closed-lab check fires before candidacy is even asked"
        );
        let mut anthropic = provider("anthropic", &["claude-sonnet-4-5"]);
        anthropic.pricing = Some(pricing(1.0, 1.0, 0.0));
        assert_eq!(
            reroute_model(
                &[anthropic, b],
                None,
                "anthropic/claude-sonnet-4-5",
                &all_healthy,
                &permissive
            ),
            None,
            "closed labs are exclusive first-party hosts: never rerouted"
        );
    }

    #[test]
    fn any_provider_marking_the_canonical_host_sensitive_blocks_the_rewrite() {
        let mut a = provider("a", &["glm-5"]);
        a.pricing = Some(pricing(1.0, 1.0, 0.0));
        let mut b = provider("b", &["glm-5"]);
        b.pricing = Some(pricing(0.1, 0.1, 0.0));
        b.host_sensitive_models = vec!["glm-5".into()];
        assert_eq!(
            reroute_model(&[a.clone(), b.clone()], None, "a/glm-5", &all_healthy, &permissive),
            None,
            "one host-sensitive mark grounds the canonical for every host"
        );
        let ranking = host_ranking(&[a, b], None, "glm-5", &all_healthy).unwrap();
        assert!(!ranking.eligible, "the ranking says what the policy did");
        let marker = ranking.hosts.iter().find(|h| h.provider_id == "b").unwrap();
        assert!(marker.host_sensitive, "the marking provider's row carries the flag");
        assert!(
            !ranking.hosts.iter().any(|h| h.provider_id == "a" && h.host_sensitive),
            "and only the marking provider's row does"
        );
        // An exact-match list: a neighbouring canonical is untouched by the mark.
        let mut c = provider("c", &["glm-5-air"]);
        c.pricing = Some(pricing(0.1, 0.1, 0.0));
        let marked = {
            let mut m = provider("m", &["glm-5"]);
            m.host_sensitive_models = vec!["glm-5".into()];
            m
        };
        let mut air = provider("z", &["glm-5-air"]);
        air.pricing = Some(pricing(1.0, 1.0, 0.0));
        assert_eq!(
            reroute_model(&[air, c, marked], None, "z/glm-5-air", &all_healthy, &permissive),
            Some("c/glm-5-air".into()),
            "the mark names one canonical, not the family"
        );
    }

    #[test]
    fn single_candidate_unknown_host_and_unpriced_fleet_never_rewrite() {
        let mut only = provider("only", &["glm-5"]);
        only.pricing = Some(pricing(0.1, 0.1, 0.0));
        assert_eq!(
            reroute_model(&[only], None, "only/glm-5", &all_healthy, &permissive),
            None,
            "nothing to choose"
        );

        // The current provider must be among the candidates: a setting naming a host that does
        // not list the canonical is the operator's data, not ours to move.
        let mut a = provider("a", &["glm-5"]);
        a.pricing = Some(pricing(1.0, 1.0, 0.0));
        let mut b = provider("b", &["glm-5"]);
        b.pricing = Some(pricing(0.1, 0.1, 0.0));
        let elsewhere = provider("elsewhere", &["other-model"]);
        assert_eq!(
            reroute_model(&[a, b, elsewhere], None, "elsewhere/glm-5", &all_healthy, &permissive),
            None
        );

        // Nothing priced anywhere: there is nothing to rank on.
        let unpriced_a = provider("a", &["glm-5"]);
        let unpriced_b = provider("b", &["glm-5"]);
        assert_eq!(
            reroute_model(&[unpriced_a, unpriced_b], None, "a/glm-5", &all_healthy, &permissive),
            None
        );
    }

    #[test]
    fn a_candidate_whose_model_map_lacks_the_canonical_is_excluded() {
        let mut a = provider("a", &["glm-5"]);
        a.pricing = Some(pricing(1.0, 1.0, 0.0));
        let mut mapped = provider("mapped", &["glm-5"]);
        mapped.model_map = BTreeMap::from([("other".to_string(), "glm-5-turbo".to_string())]);
        // Without a third server there is no ranking at all.
        assert_eq!(
            reroute_model(&[a.clone(), mapped.clone()], None, "a/glm-5", &all_healthy, &permissive),
            None
        );

        // With one, the mapped host is not a candidate — it cannot serve the canonical.
        let mut b = provider("b", &["glm-5"]);
        b.pricing = Some(pricing(0.1, 0.1, 0.0));
        let providers = [a, mapped, b];
        let ranking = host_ranking(&providers, None, "glm-5", &all_healthy).unwrap();
        assert_eq!(
            ranking.hosts.iter().map(|h| h.provider_id.as_str()).collect::<Vec<_>>(),
            ["b", "a"],
            "the non-empty model_map is authoritative"
        );
        // A map that does name the canonical keeps the host in.
        let mut named = provider("named", &["glm-5"]);
        named.model_map = BTreeMap::from([("glm-5".to_string(), "glm-5-0620".to_string())]);
        named.pricing = Some(pricing(0.05, 0.05, 0.0));
        let providers = [provider("a", &["glm-5"]), provider("b", &["glm-5"]), named];
        assert_eq!(
            reroute_model(&providers, None, "a/glm-5", &all_healthy, &permissive),
            Some("named/glm-5".into())
        );
    }

    fn feed_with(input: f64, output: f64) -> FeedEntries {
        FeedEntries::from([(
            "deepinfra".to_string(),
            BTreeMap::from([(
                "glm-5".to_string(),
                FeedEntry {
                    pricing: pricing(input, output, 0.0),
                    last_verified_at: Some("2026-10-01T00:00:00Z".into()),
                    source: Some("https://example.com/prices".into()),
                },
            )]),
        )])
    }

    #[test]
    fn a_feed_priced_host_ranks_carries_verification_and_is_choosable() {
        let feed = feed_with(0.2, 0.8);
        let mut local = provider("zai", &["glm-5"]);
        local.pricing = Some(pricing(1.0, 2.0, 0.0));
        // Priced only by the feed, under the connection's feed id.
        let mut fed = provider("fed", &["glm-5"]);
        fed.price_feed_id = Some("deepinfra".into());
        let providers = [local, fed];
        let ranking = host_ranking(&providers, Some(&feed), "glm-5", &all_healthy).unwrap();
        assert_eq!(
            ranking.hosts.iter().map(|h| h.provider_id.as_str()).collect::<Vec<_>>(),
            ["fed", "zai"],
            "the feed's cheaper price ranks first"
        );
        let fed_host = &ranking.hosts[0];
        assert_eq!(fed_host.input_per_mtok, Some(0.2));
        assert_eq!(fed_host.last_verified_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert_eq!(fed_host.source.as_deref(), Some("https://example.com/prices"));
        assert_eq!(
            ranking.hosts[1].last_verified_at, None,
            "an operator price carries no verification"
        );
        assert_eq!(
            reroute_model(&providers, Some(&feed), "zai/glm-5", &all_healthy, &permissive),
            Some("fed/glm-5".into()),
            "a feed-priced host is choosable"
        );
    }

    #[test]
    fn a_host_the_feed_does_not_price_is_listed_but_null_and_not_choosable_from_the_feed() {
        let feed = feed_with(0.2, 0.8);
        let mut local = provider("zai", &["glm-5"]);
        local.pricing = Some(pricing(1.0, 2.0, 0.0));
        // No price anywhere on this host: the feed has no entry for its feed id.
        let unpriced = provider("together", &["glm-5"]);
        let providers = [local.clone(), unpriced];
        let ranking = host_ranking(&providers, Some(&feed), "glm-5", &all_healthy).unwrap();
        assert_eq!(ranking.hosts[1].blended_per_mtok, None, "no feed entry, no price");
        assert_eq!(ranking.hosts[1].last_verified_at, None);
        // The zai price is the operator's, so no verification rides along even with a feed present.
        assert_eq!(ranking.hosts[0].source, None);
        // A missing feed entirely keeps the operator's prices working (feed-down fallback).
        assert_eq!(
            reroute_model(&providers, None, "together/glm-5", &all_healthy, &permissive),
            Some("zai/glm-5".into())
        );
    }

    #[test]
    fn eligible_flips_false_for_a_closed_model_or_a_host_sensitive_one() {
        let mut a = provider("a", &["glm-5", "grok-4"]);
        a.pricing = Some(pricing(1.0, 1.0, 0.0));
        let mut b = provider("b", &["glm-5", "grok-4"]);
        b.pricing = Some(pricing(0.5, 0.5, 0.0));
        let open = host_ranking(&[a.clone(), b.clone()], None, "glm-5", &all_healthy).unwrap();
        assert!(open.eligible);
        let closed = host_ranking(&[a, b], None, "grok-4", &all_healthy).unwrap();
        assert!(!closed.eligible, "a closed canonical is never auto-rerouted");
        assert_eq!(
            reroute_model(
                &[provider("x", &["grok-4"]), provider("y", &["grok-4"])],
                None,
                "x/grok-4",
                &all_healthy,
                &permissive
            ),
            None
        );

        let mut marked = provider("m", &["kimi-k2"]);
        marked.host_sensitive_models = vec!["kimi-k2".into()];
        let other = provider("o", &["kimi-k2"]);
        let sensitive = host_ranking(&[marked, other], None, "kimi-k2", &all_healthy).unwrap();
        assert!(!sensitive.eligible, "a host-sensitive canonical is not eligible either");
    }

    #[test]
    fn a_candidate_the_sensitivity_class_refuses_is_not_choosable() {
        // The cheapest host would win — except its class is refused, so a pricier eligible host
        // takes the rewrite instead (fix 1: the gateway's per-request 403 read into the policy).
        let mut cheap = provider("cheap-untrusted", &["glm-5"]);
        cheap.pricing = Some(pricing(0.1, 0.1, 0.0));
        let mut pricey = provider("pricey-trusted", &["glm-5"]);
        pricey.pricing = Some(pricing(1.0, 1.0, 0.0));
        let mut current = provider("zai", &["glm-5"]);
        current.pricing = Some(pricing(0.5, 0.5, 0.0));
        let providers = [current, cheap, pricey];
        let refused = |p: &Provider| p.id != "cheap-untrusted";
        assert_eq!(
            reroute_model(&providers, None, "zai/glm-5", &all_healthy, &refused),
            Some("pricey-trusted/glm-5".into()),
            "the refused cheapest host is skipped for an eligible one"
        );
        // A permissive predicate (the ranking path, no session) lets the cheapest host win.
        assert_eq!(
            reroute_model(&providers, None, "zai/glm-5", &all_healthy, &permissive),
            Some("cheap-untrusted/glm-5".into())
        );
        // Every candidate refused: nothing to move to.
        let all_refused = |_: &Provider| false;
        assert_eq!(reroute_model(&providers, None, "zai/glm-5", &all_healthy, &all_refused), None);
    }

    #[test]
    fn a_host_with_no_saved_api_key_is_not_healthy_and_not_choosable() {
        // A provider whose `auth` needs a key but has none saved is not connected (chat.rs:149):
        // folding that into the health read keeps it out of the choosable set, however cheap.
        let mut keyless = provider("keyless", &["glm-5"]);
        keyless.auth = "x-api-key".into();
        keyless.pricing = Some(pricing(0.1, 0.1, 0.0));
        let mut pricey = provider("pricey", &["glm-5"]);
        pricey.pricing = Some(pricing(1.0, 1.0, 0.0));
        let state = |p: &Provider| HostState {
            connected: p.auth == "none",
            ..healthy()
        };
        let ranking = host_ranking(&[keyless.clone(), pricey.clone()], None, "glm-5", &state).unwrap();
        let keyless_row = ranking.hosts.iter().find(|h| h.provider_id == "keyless").unwrap();
        assert!(
            !keyless_row.healthy,
            "a keyless host is reported as not healthy, however cheap"
        );
        let current = provider("zai", &["glm-5"]);
        let priced_current = {
            let mut c = current.clone();
            c.pricing = Some(pricing(0.5, 0.5, 0.0));
            c
        };
        assert_eq!(
            reroute_model(&[priced_current, keyless, pricey], None, "zai/glm-5", &state, &permissive),
            Some("pricey/glm-5".into()),
            "the keyless cheapest host is skipped for a connected pricier one"
        );
    }

    #[test]
    fn the_blend_is_comparable_when_only_some_hosts_publish_a_cache_rate() {
        // One host publishes a cache-read rate, one does not. The cache-aware form would hand the
        // publishing host a discount the other can never earn — so the plain 3:1 form is used for
        // both, and the genuinely cheaper host wins (fix 5).
        let mut publishing = provider("publishing", &["glm-5"]);
        publishing.pricing = Some(pricing(1.0, 1.0, 0.1));
        let mut plain = provider("plain", &["glm-5"]);
        plain.pricing = Some(pricing(0.5, 0.5, 0.0));
        let ranking = host_ranking(&[publishing, plain], None, "glm-5", &all_healthy).unwrap();
        // plain's 3:1 blend is (3·0.5 + 0.5)/4 = 0.5; publishing's is (3·1 + 1)/4 = 1.0 — not the
        // cache-aware 0.775 it would have earned on its own.
        let plain_host = ranking.hosts.iter().find(|h| h.provider_id == "plain").unwrap();
        let publishing_host = ranking.hosts.iter().find(|h| h.provider_id == "publishing").unwrap();
        assert!(close(plain_host.blended_per_mtok.unwrap(), 0.5));
        assert!(close(publishing_host.blended_per_mtok.unwrap(), 1.0));
        assert_eq!(
            ranking.hosts[0].provider_id, "plain",
            "the cheaper host on both published rates wins, no cache discount handed out"
        );

        // When every priced host publishes a cache-read rate, the cache-aware form is used.
        let mut publishing_a = provider("pub-a", &["glm-5"]);
        publishing_a.pricing = Some(pricing(1.0, 1.0, 0.1));
        let mut publishing_b = provider("pub-b", &["glm-5"]);
        publishing_b.pricing = Some(pricing(1.0, 1.0, 0.5));
        let cache_ranking = host_ranking(&[publishing_a, publishing_b], None, "glm-5", &all_healthy).unwrap();
        let a = cache_ranking.hosts.iter().find(|h| h.provider_id == "pub-a").unwrap();
        assert!(close(a.blended_per_mtok.unwrap(), (2.0 * 1.0 + 0.1 + 1.0) / 4.0));
    }
}
