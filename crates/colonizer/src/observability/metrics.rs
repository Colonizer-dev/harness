//! The Prometheus surface of the `observability` module kind: `GET /metrics` and the catalogue it
//! serves (#852).
//!
//! One scrape reads the mothership's own in-memory state and renders the Prometheus text exposition
//! format 0.0.4 by hand — no new crate, no registry, and nothing kept between scrapes except the
//! counters below. Three rules shape everything here:
//!
//! 1. **No colony id, no free text, no `repo` label.** An id is unbounded (one series per colony
//!    would bury the scrape), and the ADR's cardinality rule ([`docs/design/observability.md`]) keeps
//!    per-repo series off Loki labels for the same reason. What a series carries is an org, a
//!    provider, a model, an agent, a status or a reason — codes, sanitised and length-capped, never
//!    a sentence.
//! 2. **Bounded cardinality, twice over.** Every label value is sanitised and capped at
//!    [`MAX_LABEL`] characters, and every family stops at [`SERIES_CAP`] distinct label sets, folding
//!    the rest into one `other` series. Ten thousand orgs cost 201 series, not ten thousand.
//! 3. **The hash is per install, not per scrape.** `repo_names = hashed` replaces an org with
//!    `hmac-sha256(<data>/observability/hash.key, org)[..12]`, so the same org reads the same across
//!    scrapes and across a fleet that has copied the key, while the plaintext never leaves. A
//!    missing key is created with 32 random bytes at mode 0600; a key that can be neither read nor
//!    created is retried per scrape and then answered `503`, because a scrape in which every org
//!    reads `unknown` is a real number that says nothing.
//!
//! Auth is the install-wide **read** token, never the OTLP headers secret (ADR auth rule 5): an
//! owner, or a `read`/`operate`/`launch` token with no org or repo limits, gets the catalogue. A
//! limited token is a 403 rather than a filtered view, because a *filtered* metrics page is a lie —
//! the series that were dropped are the ones an operator needs to see. A `fleet` token is refused
//! for the same reason it cannot read the owner's other colonies. With no token at all `host_guard`
//! has already answered 401, and a missing or switched-off module answers 404, so a scraped
//! endpoint never appears on a build that was never configured for it.
//!
//! The split is by direction: this file owns the state, the counters and the route;
//! [`exposition`] owns the format — label safety, the family cap, the org hash and the rendering.
//! It is all pure, so it can be tested without an app.

mod exposition;

use std::{
    collections::BTreeMap,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use axum::{
    extract::{Extension, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::api_tokens::{Scope, ScopedToken};
use crate::observability::settings::ExporterConfig;
use crate::{Shared, sessions::Session, spend};

use exposition::{Family, Hasher, HistogramFamily, label, load_or_create_key};

/// The path the catalogue is served at, the conventional Prometheus scrape target.
pub(crate) const PATH: &str = "/metrics";
/// The one content type Prometheus 0.0.4 exposition is served as.
pub(crate) const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// A label value longer than this is cut: long enough for a code, short enough that a paste can
/// never become a series name.
const MAX_LABEL: usize = 64;
/// How many distinct label sets one family may carry. Beyond it every further set folds into a
/// single `other` series, so a busy install cannot make the scrape unbounded.
const SERIES_CAP: usize = 200;
/// The label value a folded set takes.
const OTHER: &str = "other";

/// The `outcome.*` suffixes `colonizer_colonies_finished_total` counts, in the order the static
/// array below holds them. A closed set on purpose: an unknown suffix is not counted rather than
/// inventing a series per new kind.
pub(crate) const OUTCOMES: [&str; 9] = [
    "pr_opened",
    "merged",
    "closed",
    "no_changes",
    "stopped",
    "failed",
    "question",
    "suspended",
    "restored",
];

/// Every reason a colony can be flagged for, and the only ones that may label a series.
///
/// The flag itself is free-form JSON — a `reason` handed down from a caller, an error string, a
/// half-finished edit — and a scrape must never carry a sentence. So the list is closed at both
/// ends: the constants each subsystem already writes its own flag with are spelled out where they
/// are public, and anything outside the set is reported as [`OTHER`]. A reason nobody has written
/// today would still have a series waiting for it, which is the cheaper mistake: an under-used
/// `other` costs one aggregate, a leaked sentence costs the install its log.
pub(crate) const ATTENTION_REASONS: [&str; 13] = [
    // Written as these constants today.
    crate::autonomy::ALERT_REASON,
    crate::queue::AUTOPILOT_HELD_REASON,
    crate::queue::HOLD_TIMEOUT_REASON,
    crate::queue::ABANDONED_QUESTION_REASON,
    crate::queue::PROVIDER_RETRY_REASON,
    crate::queue::HOLD_UNANSWERED_REASON,
    crate::events::AGENT_FAILED,
    crate::gateway::MODEL_ERROR_REASON,
    crate::account_health::WAITING_FOR_ACCOUNT_REASON,
    crate::provider_quota::QUOTA_EXHAUSTED_REASON,
    // Written as literals at the few sites that predate the constants.
    "stalled",
    "nudges_exhausted",
    "waiting_for_answer",
];

/// The label a colony's attention `reason` is reported under: the reason itself when it is one this
/// build writes, and [`OTHER`] when it is anything else. Pure, and the only place the rule lives.
fn attention_reason(raw: &str) -> &str {
    if ATTENTION_REASONS.contains(&raw) { raw } else { OTHER }
}

/// The activity kind that starts a colony.
const LAUNCH: &str = "colony.launch";
/// The prefix every outcome kind carries.
const OUTCOME_PREFIX: &str = "outcome.";

// ---------------------------------------------------------------------------
// The counters that outlive a scrape.
//
// All process globals, on the one-app-per-process assumption every other static in the crate
// already makes: `COLONIES_STARTED`, `COLONIES_FINISHED`, `SEEDED` and `HASH_KEY` are the install's,
// because the activity log and the data dir they are seeded from are too. A second `App` in one
// process would share them — which is exactly what the tests in this file rely on, and exactly
// what would be wrong in a real daemon hosting two installs. There is none.
// ---------------------------------------------------------------------------

/// Colonies started since the counters were seeded. Seeded once from the activity log and bumped
/// live by [`on_activity`]; see [`SEEDED`] for why the two cannot double count.
static COLONIES_STARTED: AtomicU64 = AtomicU64::new(0);
/// Colonies finished, one counter per entry of [`OUTCOMES`].
static COLONIES_FINISHED: [AtomicU64; OUTCOMES.len()] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
/// Whether the start-up seed has run. Until it has, a live bump is *not* counted, because the seed
/// is about to `store` the log's own count over it — counting both would double the launches that
/// happened in that window. The cost is the opposite and much smaller one: a launch recorded in the
/// window is counted by the next start's seed, so the counter is briefly low, never double.
static SEEDED: AtomicBool = AtomicBool::new(false);
/// The hash key for `repo_names = hashed`, loaded once at start and retried per scrape while it
/// is absent. A mutex rather than a `OnceLock`, precisely so a failed start is not the final word.
static HASH_KEY: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// The key a scrape hashes with, if it has one.
fn hash_key() -> Option<Vec<u8>> {
    HASH_KEY.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Parks a loaded key, or records that there is none. A `None` here is a statement about now, not a
/// permanent verdict: the next scrape loads again.
fn set_hash_key(key: Option<Vec<u8>>) {
    *HASH_KEY.lock().unwrap_or_else(|p| p.into_inner()) = key;
}

/// Counts one recorded activity line towards the two colony counters (#852).
///
/// A single `fetch_add` on the path that appends every activity line, and nothing else: no lock, no
/// allocation, no read of the log. Called from `activity::record`; kinds that are neither a launch
/// nor a known outcome cost one string compare.
pub(crate) fn on_activity(kind: &str) {
    if !SEEDED.load(Ordering::Acquire) {
        return;
    }
    if kind == LAUNCH {
        COLONIES_STARTED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if let Some(suffix) = kind.strip_prefix(OUTCOME_PREFIX)
        && let Some(at) = OUTCOMES.iter().position(|o| *o == suffix)
    {
        COLONIES_FINISHED[at].fetch_add(1, Ordering::Relaxed);
    }
}

/// The two colony counters, read for a scrape.
fn colony_counters() -> (u64, Vec<u64>) {
    (
        COLONIES_STARTED.load(Ordering::Relaxed),
        COLONIES_FINISHED.iter().map(|c| c.load(Ordering::Relaxed)).collect(),
    )
}

// ---------------------------------------------------------------------------
// The gateway latency histogram.
// ---------------------------------------------------------------------------

/// A request duration, as milliseconds. Seconds at the exposition, because that is what a
/// Prometheus histogram is read in; the whole conversion happens in the renderer.
pub(crate) type Millis = u64;

/// The bucket upper bounds, in milliseconds: a tenth of a second to five minutes, wide where a
/// provider call is expected to be slow and narrow where a fast one should be distinguishable. The
/// count is fixed, so a scrape's bucket set never moves under a dashboard.
pub(crate) const LE_MS: [Millis; 11] = [100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000, 60_000, 120_000, 300_000];
/// The same bounds as the `le` label values a scrape prints, in seconds. Written out rather than
/// formatted so the exposition is byte-identical every time.
const LE_LABELS: [&str; LE_MS.len()] = ["0.1", "0.25", "0.5", "1", "2.5", "5", "10", "30", "60", "120", "300"];

/// Cumulative counts, sum and total for one histogram, as one scrape reads them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HistogramSnapshot {
    /// One counter per entry of [`LE_MS`], each counting only the observations that fell in it and
    /// not above; the renderer accumulates them into the cumulative `le` buckets Prometheus reads.
    pub(crate) buckets: [u64; LE_MS.len()],
    /// Every observation's duration, summed.
    pub(crate) sum_ms: u64,
    /// How many observations there were, the `_count` line and the `+Inf` bucket alike.
    pub(crate) count: u64,
}

/// A request-latency histogram for one provider: fixed buckets, summed, counted.
///
/// Every write is a `fetch_add` on its own atomic and every read is a `load`, so a request pays no
/// lock — the histogram is dropped in by the gateway's own `Drop` guard, beside the `duration_ms`
/// add it already did. The cost is a fixed array per provider and eleven relaxed atomics per
/// request.
#[derive(Debug)]
pub(crate) struct Histogram {
    buckets: [AtomicU64; LE_MS.len()],
    sum_ms: AtomicU64,
    count: AtomicU64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; LE_MS.len()],
            sum_ms: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }
}

impl Histogram {
    /// Records one request that took `ms` milliseconds.
    pub(crate) fn observe(&self, ms: Millis) {
        for (at, bound) in LE_MS.iter().enumerate() {
            if ms <= *bound {
                self.buckets[at].fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        self.sum_ms.fetch_add(ms, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// The counters as one scrape reads them.
    pub(crate) fn snapshot(&self) -> HistogramSnapshot {
        let mut buckets = [0u64; LE_MS.len()];
        for (at, bucket) in self.buckets.iter().enumerate() {
            buckets[at] = bucket.load(Ordering::Relaxed);
        }
        HistogramSnapshot {
            buckets,
            sum_ms: self.sum_ms.load(Ordering::Relaxed),
            count: self.count.load(Ordering::Relaxed),
        }
    }
}

/// Two histograms summed, so an overflow of providers folds into one `other` series without
/// inventing a rate the series never had.
fn merge(a: HistogramSnapshot, b: HistogramSnapshot) -> HistogramSnapshot {
    let mut buckets = a.buckets;
    for (at, value) in b.buckets.iter().enumerate() {
        buckets[at] += value;
    }
    HistogramSnapshot {
        buckets,
        sum_ms: a.sum_ms + b.sum_ms,
        count: a.count + b.count,
    }
}

// ---------------------------------------------------------------------------
// The snapshot, collected under short read guards.
// ---------------------------------------------------------------------------

/// Everything one scrape needs, as plain owned data. Collected with the app's locks held only long
/// enough to read, then dropped before any hashing or rendering happens.
struct Snapshot {
    /// One `(status, org, agent, count)` per live combination.
    colonies: Vec<(String, String, String, u64)>,
    /// Colonies started and finished, from the process-static counters.
    started: u64,
    finished: Vec<u64>,
    /// One gateway sample per provider that already has state, read without inserting.
    providers: Vec<crate::gateway::ProviderScrape>,
    /// Per org per model `(org, model, tokens)` and the org's four token totals.
    tokens: Vec<(String, String, &'static str, u64)>,
    /// `(org, model, cost)` where a model had a cost attributed.
    cost: Vec<(String, String, f64)>,
    /// Colonies waiting for an answer.
    questions_open: u64,
    /// Colonies flagged for attention, by reason.
    attention: Vec<(String, u64)>,
    /// Colonies queued for a slot.
    queue_depth: u64,
    /// Whether a write or config-read alert is showing.
    storage_alert: bool,
    /// Free bytes on the data volume, `None` while it has never been measured.
    disk_free_bytes: Option<u64>,
}

impl Snapshot {
    /// Reads the install's state. Every guard is taken and dropped here; nothing below this point
    /// touches the app.
    async fn collect(app: &Shared) -> Snapshot {
        use crate::sessions::SessionStatus;

        let mut colonies: BTreeMap<(String, String, String), u64> = BTreeMap::new();
        let mut by_org: BTreeMap<String, Vec<&Session>> = BTreeMap::new();
        let mut questions_open = 0u64;
        let mut queue_depth = 0u64;
        let mut attention: BTreeMap<String, u64> = BTreeMap::new();
        let mut tokens = Vec::new();
        let mut cost = Vec::new();
        {
            // One read of the live list, held across the counting and the rollup: the rollup takes
            // `&Session`, and both are pure arithmetic over what is already in memory — no await,
            // no I/O — so the guard is released before any hashing or rendering happens.
            let sessions = app.sessions.read().await;
            for session in sessions.iter() {
                *colonies
                    .entry((
                        session.status.as_str().to_string(),
                        session.org.clone(),
                        session.agent.clone(),
                    ))
                    .or_default() += 1;
                match session.status {
                    SessionStatus::WaitingForAnswer => questions_open += 1,
                    SessionStatus::Queued => queue_depth += 1,
                    _ => {}
                }
                if let Some(flag) = &session.attention
                    && let Some(reason) = flag.get("reason").and_then(serde_json::Value::as_str)
                {
                    let reason = attention_reason(reason);
                    *attention.entry(reason.to_string()).or_default() += 1;
                }
                by_org.entry(session.org.clone()).or_default().push(session);
            }
            for (org, of_org) in by_org {
                let roll = spend::rollup_sessions(of_org);
                for (model, model_spend) in &roll.models {
                    tokens.push((org.clone(), model.clone(), "total", model_spend.tokens));
                    if let Some(usd) = model_spend.cost_usd {
                        cost.push((org.clone(), model.clone(), usd));
                    }
                }
                for (kind, count) in [
                    ("input", roll.input_tokens),
                    ("output", roll.output_tokens),
                    ("cache_read", roll.cache_read_tokens),
                    ("cache_write", roll.cache_write_tokens),
                ] {
                    tokens.push((org.clone(), "all".to_string(), kind, count));
                }
            }
        }
        // One non-inserting read of the gateway: a scrape must not create a provider entry.
        let providers = app.gateway.scrape_stats();
        let (started, finished) = colony_counters();
        Snapshot {
            colonies: colonies
                .into_iter()
                .map(|((status, org, agent), count)| (status, org, agent, count))
                .collect(),
            started,
            finished,
            providers,
            tokens,
            cost,
            questions_open,
            attention: attention.into_iter().collect(),
            queue_depth,
            storage_alert: app.shown_storage_alert().await.is_some(),
            disk_free_bytes: app.disk_verdict.lock().await.free_bytes,
        }
    }
}

// ---------------------------------------------------------------------------
// The catalogue.
// ---------------------------------------------------------------------------

/// Renders the whole catalogue in the 0.0.4 text format.
fn render(snapshot: &Snapshot, hasher: &Hasher) -> String {
    let mut out = String::with_capacity(16 * 1024);

    // Colonies: the gauge that answers "what is this install doing right now".
    let mut colonies = Family::gauge(
        "colonizer_colonies",
        "Colonies by current status, organisation and agent.",
        &["status", "org", "agent"],
    );
    for (status, org, agent, count) in &snapshot.colonies {
        colonies.push(vec![label(status), hasher.org(org), label(agent)], *count as f64);
    }
    colonies.render(&mut out);

    // The two lifetime counters.
    let mut started = Family::counter(
        "colonizer_colonies_started_total",
        "Colonies started since this install's counters were seeded.",
        &[],
    );
    started.push(Vec::new(), snapshot.started as f64);
    started.render(&mut out);

    let mut finished = Family::counter(
        "colonizer_colonies_finished_total",
        "Colonies finished, by outcome.",
        &["outcome"],
    );
    for (at, outcome) in OUTCOMES.iter().enumerate() {
        finished.push(vec![label(outcome)], snapshot.finished[at] as f64);
    }
    finished.render(&mut out);

    // The gateway: how much traffic each provider carried, and how it went.
    let mut requests = Family::counter(
        "colonizer_gateway_requests_total",
        "Gateway requests by provider, split ok and error.",
        &["provider", "outcome"],
    );
    let mut fallbacks = Family::counter(
        "colonizer_gateway_fallbacks_total",
        "Gateway responses that fell back to another model.",
        &["provider"],
    );
    let mut in_flight = Family::gauge(
        "colonizer_gateway_in_flight",
        "Gateway requests streaming right now, by provider.",
        &["provider"],
    );
    let mut queued = Family::gauge(
        "colonizer_gateway_queued",
        "Gateway requests waiting for a provider slot, by provider.",
        &["provider"],
    );
    for sample in &snapshot.providers {
        let provider = label(&sample.provider);
        let errors = sample.usage.failures;
        requests.push(vec![provider.clone(), label("error")], errors as f64);
        requests.push(
            vec![provider.clone(), label("ok")],
            sample.usage.requests.saturating_sub(errors) as f64,
        );
        fallbacks.push(vec![provider.clone()], sample.usage.fallbacks as f64);
        in_flight.push(vec![provider.clone()], sample.in_flight as f64);
        queued.push(vec![provider], sample.queued as f64);
    }
    requests.render(&mut out);
    fallbacks.render(&mut out);
    in_flight.render(&mut out);
    queued.render(&mut out);

    let mut durations = HistogramFamily::new(
        "colonizer_gateway_request_duration_seconds",
        "Gateway request duration in seconds, by provider.",
    );
    for sample in &snapshot.providers {
        durations.push(&label(&sample.provider), sample.latency);
    }
    durations.render(&mut out);

    // Tokens and cost. `model="all"` with a `type` and `model=<name>` with `type="total"` are two
    // views of the same tokens and are never summed together — see the docs page.
    let mut token_family = Family::counter(
        "colonizer_tokens_total",
        "Tokens spent. model=\"all\" carries the four per-kind totals; a named model carries that model's total.",
        &["org", "model", "type"],
    );
    for (org, model, kind, count) in &snapshot.tokens {
        token_family.push(vec![hasher.org(org), label(model), label(kind)], *count as f64);
    }
    token_family.render(&mut out);

    let mut cost_family = Family::counter(
        "colonizer_cost_usd_total",
        "Cost in US dollars, by organisation and model. A model nothing priced yet has no series.",
        &["org", "model"],
    );
    for (org, model, usd) in &snapshot.cost {
        cost_family.push(vec![hasher.org(org), label(model)], *usd);
    }
    cost_family.render(&mut out);

    // The state gauges.
    let mut questions = Family::gauge("colonizer_questions_open", "Colonies waiting for an answer.", &[]);
    questions.push(Vec::new(), snapshot.questions_open as f64);
    questions.render(&mut out);

    let mut attention = Family::gauge(
        "colonizer_attention",
        "Colonies flagged for attention, by reason.",
        &["reason"],
    );
    for (reason, count) in &snapshot.attention {
        attention.push(vec![label(reason)], *count as f64);
    }
    attention.render(&mut out);

    let mut queue = Family::gauge("colonizer_queue_depth", "Colonies waiting for a free slot.", &[]);
    queue.push(Vec::new(), snapshot.queue_depth as f64);
    queue.render(&mut out);

    let mut storage = Family::gauge(
        "colonizer_storage_alert",
        "1 while a write or config-read alert is showing, else 0.",
        &[],
    );
    storage.push(Vec::new(), u64::from(snapshot.storage_alert) as f64);
    storage.render(&mut out);

    let mut disk = Family::gauge(
        "colonizer_disk_free_bytes",
        "Free bytes on the data volume. No series while it has never been measured.",
        &[],
    );
    if let Some(free) = snapshot.disk_free_bytes {
        disk.push(Vec::new(), free as f64);
    }
    disk.render(&mut out);

    out
}

// ---------------------------------------------------------------------------
// The route.
// ---------------------------------------------------------------------------

/// The routes this module serves; merged by `server::api_routes` (through `features::routes()`).
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    axum::Router::new().route(PATH, axum::routing::get(scrape))
}

/// What a scoped token needs for `/metrics`: read scope, nothing else. `host_guard` runs before the
/// router matches, so the path's segments are what the rule sees, exactly as `api_tokens::classify`
/// sees them.
fn token_scope<'a>(method: &axum::http::Method, segs: &[&'a str]) -> Option<crate::api_tokens::Need<'a>> {
    match segs {
        [segment] if *segment == PATH.trim_start_matches('/') && *method == axum::http::Method::GET => {
            Some(crate::api_tokens::Need::Bare(Scope::Read))
        }
        _ => None,
    }
}

/// `GET /metrics`: the catalogue, in the 0.0.4 text format.
///
/// 404 when there is no observability module, it is switched off, or `prometheus` is off — an
/// endpoint an operator never configured should not exist to be found. 403 for a paired phone,
/// and for a token limited to some orgs or repos or holding fleet scope, all of which reach a
/// bare-read route `host_guard` has already let past: the catalogue is the whole install, and a
/// filtered page would hide the very series that are out of reach. 503 when `repo_names = hashed`
/// is on and the hash key cannot be loaded, because the alternative is a scrape of `org="unknown"`
/// for every org — a real number that says nothing, and one a dashboard would cache.
pub async fn scrape(
    State(app): State<Shared>,
    token: Option<Extension<ScopedToken>>,
    phone: Option<Extension<crate::phone::PhoneDevice>>,
) -> Response {
    let config = {
        let modules = app.modules.read().await;
        modules.observability.as_ref().and_then(ExporterConfig::from_module)
    };
    let Some(config) = config.filter(|c| c.prometheus) else {
        return crate::client_error(StatusCode::NOT_FOUND, "metrics are not enabled on this install").into_response();
    };
    // A paired phone authenticates as the owner in `host_guard` and may make any GET
    // (`phone::phone_may`). The cockpit is a phone's whole surface, though: the install's own
    // numbers are the owner's, and a phone is a revocable credential that lives on a device.
    if phone.is_some() {
        return crate::client_error(
            StatusCode::FORBIDDEN,
            "/metrics is the install's own state, and a phone is not the owner; scrape it from the computer",
        )
        .into_response();
    }
    if let Some(token) = &token
        && (token.scope == Scope::Fleet || !token.orgs.is_empty() || !token.repos.is_empty())
    {
        return crate::client_error(
            StatusCode::FORBIDDEN,
            "this API token is limited to some organisations or repositories, and /metrics is install-wide; \
             use a token with no limits",
        )
        .into_response();
    }
    let hashed = config.repo_names == "hashed";
    // The key is loaded once at start, but a start that raced the data dir, or a key file that was
    // not readable then, must not break every later scrape: a hashed install with no key is
    // retried here, in `spawn_blocking`, and only gives up if this attempt fails too.
    let key = if hashed && hash_key().is_none() {
        let data_dir = app.cfg.data_dir.clone();
        let loaded = tokio::task::spawn_blocking(move || load_or_create_key(&data_dir))
            .await
            .unwrap_or(None);
        set_hash_key(loaded.clone());
        loaded
    } else {
        hash_key()
    };
    if hashed && key.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "repo_names is hashed but the hash key is unavailable",
        )
            .into_response();
    }
    let snapshot = Snapshot::collect(&app).await;
    let body = tokio::task::spawn_blocking(move || render(&snapshot, &Hasher::new(hashed, key)))
        .await
        .unwrap_or_default();
    ([(header::CONTENT_TYPE, CONTENT_TYPE)], body).into_response()
}

/// This module's feature descriptor (`features.rs`).
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "observability",
    routes,
    token_scope: Some(token_scope),
    activity: &[],
    kinds: &[],
    start_tasks: Some(start_tasks),
};

/// Seeds the two colony counters from the activity log and loads the hash key (#852). Both are
/// file work, so both run in `spawn_blocking` and neither touches the reactor.
pub(crate) fn start_tasks(app: &Shared) {
    let data_dir = app.cfg.data_dir.clone();
    tokio::spawn(async move {
        let for_seed = data_dir.clone();
        let seeded = tokio::task::spawn_blocking(move || seed_from_log(&for_seed))
            .await
            .unwrap_or_default();
        apply_seed(seeded);
        let key = tokio::task::spawn_blocking(move || load_or_create_key(&data_dir))
            .await
            .unwrap_or(None);
        set_hash_key(key);
    });
}

/// Launches and outcomes the activity log already holds, as `(started, finished)`.
fn seed_from_log(data_dir: &Path) -> (u64, Vec<u64>) {
    let (entries, _skipped) = crate::activity::read_all(data_dir);
    let mut started = 0u64;
    let mut finished = vec![0u64; OUTCOMES.len()];
    for entry in entries {
        if entry.kind == LAUNCH {
            started += 1;
        } else if let Some(suffix) = entry.kind.strip_prefix(OUTCOME_PREFIX)
            && let Some(at) = OUTCOMES.iter().position(|o| *o == suffix)
        {
            finished[at] += 1;
        }
    }
    (started, finished)
}

/// Publishes the seed and only then lets live bumps count, so the log's own count and the live
/// `fetch_add`s can never both describe the same line.
fn apply_seed((started, finished): (u64, Vec<u64>)) {
    COLONIES_STARTED.store(started, Ordering::Release);
    for (at, value) in finished.iter().enumerate() {
        if let Some(counter) = COLONIES_FINISHED.get(at) {
            counter.store(*value, Ordering::Release);
        }
    }
    SEEDED.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests;
