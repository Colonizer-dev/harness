//! The features that carry their own registration.
//!
//! A migrated feature keeps its routes, its scoped-token rule, its activity rules and its
//! background work beside its handlers, in one [`Feature`] descriptor, and lists itself in [`ALL`].
//! `server::api_routes`, `server::start_tasks`, `api_tokens::classify` and `activity::rule_for`
//! read this list instead of a central line each, so two features never edit the same list: two
//! parallel pull requests to features that are not alphabetical neighbours do not conflict, and
//! adding a feature is one line here plus its `mod` line in `main.rs`.
//!
//! Modules not migrated yet still register in the older central lists (`server.rs`,
//! `api_tokens.rs`, `activity.rs`); a module joins this file when someone moves it over.
//! `docs/decisions.md` records why this is a hand-kept list rather than an `inventory` one.

use axum::http::Method;

use crate::api_tokens::Need;

/// A feature's scoped-token rule: given the method and the path's segments, exactly as
/// `api_tokens::classify` sees them, what a scoped token needs — or `None` to leave the route to
/// the legacy arms.
pub(crate) type TokenScope = for<'a> fn(&Method, &[&'a str]) -> Option<Need<'a>>;

/// One migrated feature: everything the mothership needs to know about it, next to its handlers.
pub(crate) struct Feature {
    /// The feature's name, labelling this file's `ALL` line and read by the guard tests below.
    #[cfg_attr(not(test), allow(dead_code))]
    pub name: &'static str,
    /// Its API routes, merged into `server::api_routes`.
    pub routes: fn() -> axum::Router<crate::Shared>,
    /// What a scoped token needs for its routes, or `None` when it has none of its own — its routes
    /// then fall to the legacy `classify` arms and are owner-only by default.
    pub token_scope: Option<TokenScope>,
    /// The activity rules its routes record; `activity::rule_for` falls through to these after the
    /// central `RULES`.
    pub activity: &'static [crate::activity::Rule],
    /// The activity kinds only this feature writes, added after `activity::KINDS`.
    pub kinds: &'static [&'static str],
    /// Its background work, started with every other module's by `server::start_tasks`.
    pub start_tasks: Option<fn(&crate::Shared)>,
}

/// Every migrated feature, one line each, in alphabetical order. The order changes nothing — the
/// router merges distinct routes and `classify` asks each feature for its own — it just keeps the
/// list greppable and the conflicts between alphabetical neighbours.
pub(crate) const ALL: &[&Feature] = &[
    &crate::auto_colonize::FEATURE,
    &crate::built_with::FEATURE,
    &crate::cratefield_push::FEATURE,
    &crate::decisions::FEATURE,
    &crate::github_breaker::FEATURE,
    &crate::handoff::FEATURE,
    &crate::history::FEATURE,
    &crate::loop_history::FEATURE,
    &crate::maps::FEATURE,
    &crate::merge_steward::FEATURE,
    &crate::model_switch::FEATURE,
    &crate::observability::FEATURE,
    &crate::provider_history::FEATURE,
    &crate::public_feed::FEATURE,
    &crate::queue_priority::FEATURE,
    &crate::quota_cards::FEATURE,
    &crate::setup_state::FEATURE,
    &crate::supply_chain_loop::FEATURE,
    &crate::switch_agent::FEATURE,
    &crate::vault::FEATURE,
    &crate::notify::subscriptions::FEATURE,
];

/// Every migrated feature's routes, merged into the one router `server::api_routes` assembles.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    ALL.iter()
        .fold(axum::Router::new(), |router, feature| router.merge((feature.routes)()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::Request,
        http::{StatusCode, header},
    };
    use tower::ServiceExt as _;

    #[test]
    fn the_feature_list_is_sorted_and_unique() {
        let names: Vec<&str> = ALL.iter().map(|f| f.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "features::ALL must be sorted by name and unique");
    }

    /// A concrete request path for a route template: each `{param}` becomes `x`, as
    /// `route_table_tests` does.
    fn concrete(template: &str) -> String {
        template
            .split('/')
            .map(|seg| if seg.starts_with('{') { "x" } else { seg })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Every activity rule a feature declares names a route the feature itself registers: its
    /// method is one the route answers to. Runtime, through the feature's own router, the way
    /// `route_table_tests` reads the real one: an `OPTIONS` request reaches the method fallback,
    /// whose `Allow` header lists the registered methods.
    #[tokio::test]
    async fn every_feature_rule_names_a_route_the_feature_registers() {
        let root = std::env::temp_dir().join(format!("colonizer-features-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        for feature in ALL {
            let router = (feature.routes)().with_state(app.clone());
            for r in feature.activity {
                let uri = concrete(r.route);
                let req = Request::builder()
                    .method(Method::OPTIONS)
                    .uri(&uri)
                    .header(header::HOST, "127.0.0.1:7878")
                    .body(Body::empty())
                    .unwrap();
                let res = router.clone().oneshot(req).await.unwrap();
                assert_eq!(
                    res.status(),
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{}: OPTIONS {uri} reached a handler",
                    feature.name
                );
                let allow = res
                    .headers()
                    .get(header::ALLOW)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default();
                assert!(
                    allow.split(',').map(str::trim).any(|m| m == r.method),
                    "{}: {} does not answer {}, only {allow}",
                    feature.name,
                    r.route,
                    r.method
                );
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
