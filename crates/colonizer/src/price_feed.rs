//! The price feed (issue #1038): model prices pulled from one operator-configured URL, so
//! connections the operator never hand-priced still cost something a budget can count. The URL
//! lives in `<config_dir>/price-feed.json`; the last good copy is `<data_dir>/price-feed-cache.json`.
//! A feed is an observation, like model discovery, so it never rewrites `providers.json`: it is the
//! *lowest* rung of the pricing precedence (`Provider::price_for`), read-only under whatever the
//! operator set. A feed that goes away — unreachable, garbage, an HTTP 500 — keeps its last good
//! copy and records `last_error`: a missing feed must degrade to the operator's prices, never
//! silently to $0.

use crate::{ApiResult, App, AppError, Shared, client_error, providers::Pricing};
use axum::{Json, extract::State, http::StatusCode, routing};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

/// How old the stored copy may get before the sweep fetches the feed again.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the sweep wakes to check the copy's age.
const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);
/// A cap on the feed body, so a runaway URL cannot balloon the cache.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// How far past its `last_verified_at` a price is shown `stale` in the providers list.
const STALE_PRICE_AFTER: chrono::Duration = chrono::Duration::days(14);

/// One model's price from the feed, with its provenance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct FeedEntry {
    pub pricing: Pricing,
    /// The moment the feed last vouched for the price, normalized to UTC RFC 3339; `None` when the
    /// feed named nothing parseable, which counts as never verified — hence stale.
    pub last_verified_at: Option<String>,
    /// Where the feed says the price came from, passed through verbatim.
    pub source: Option<String>,
}

impl FeedEntry {
    /// Whether the price is too old to quote: more than [`STALE_PRICE_AFTER`] past its
    /// `last_verified_at`, or with none given.
    pub(crate) fn stale(&self) -> bool {
        self.last_verified_at
            .as_deref()
            .and_then(verified_at)
            .is_none_or(|at| Utc::now() - at > STALE_PRICE_AFTER)
    }
}

/// The feed's prices, keyed by the feed's own provider id (which a connection maps to via
/// [`crate::providers::Provider::feed_provider_id`]) and then by model.
pub(crate) type FeedEntries = BTreeMap<String, BTreeMap<String, FeedEntry>>;

/// The last good copy, persisted so a restart does not drop every price until the first fetch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Cached {
    url: String,
    etag: Option<String>,
    fetched_at: Option<DateTime<Utc>>,
    #[serde(default)]
    entries: Arc<FeedEntries>,
}

/// The operator's switch, persisted as `<config_dir>/price-feed.json`. An absent or empty URL is
/// off, the default.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Setting {
    #[serde(default)]
    url: String,
}

/// What the in-memory snapshot holds: the configured URL, the copy that belongs to it (always, by
/// construction), and how the last fetch went. Swapped whole behind the lock, so a reader gets a
/// consistent view and pays one `Arc` clone.
#[derive(Clone, Debug, Default)]
struct Snapshot {
    url: Option<String>,
    last_error: Option<String>,
    copy: Option<Arc<Cached>>,
}

/// The feed: its two files and the snapshot behind them. Cheap to read from the request path and
/// the gateway; written only by `set_url` and the fetch path.
pub(crate) struct PriceFeed {
    setting_file: PathBuf,
    cache_file: PathBuf,
    /// Swapped whole on write, so a reader clones one `Snapshot` of small fields and `Arc`s.
    state: RwLock<Snapshot>,
}

impl PriceFeed {
    /// Reads the setting and the last good copy. A copy kept for a URL other than the configured one
    /// is not this feed's prices, so it is dropped; the first fetch refills.
    pub(crate) fn load(config_dir: &Path, data_dir: &Path) -> Self {
        let url = std::fs::read(config_dir.join("price-feed.json"))
            .ok()
            .and_then(|data| serde_json::from_slice::<Setting>(&data).ok())
            .map(|setting| setting.url)
            .filter(|url| !url.is_empty());
        let cached = std::fs::read(data_dir.join("price-feed-cache.json"))
            .ok()
            .and_then(|data| serde_json::from_slice::<Cached>(&data).ok())
            .filter(|cached| !cached.url.is_empty() && Some(&cached.url) == url.as_ref());
        Self {
            setting_file: config_dir.join("price-feed.json"),
            cache_file: data_dir.join("price-feed-cache.json"),
            state: RwLock::new(Snapshot {
                url,
                last_error: None,
                copy: cached.map(Arc::new),
            }),
        }
    }

    /// The current price table, as an `Arc` so the gateway's per-request read never copies entries.
    pub(crate) fn entries(&self) -> Arc<FeedEntries> {
        let snapshot = self.state.read().unwrap();
        snapshot.copy.as_ref().map(|copy| copy.entries.clone()).unwrap_or_default()
    }

    /// The feed's status, the body GET and PUT both answer with.
    pub(crate) fn status(&self) -> Value {
        let snapshot = self.state.read().unwrap().clone();
        json!({
            "url": snapshot.url,
            "fetched_at": snapshot
                .copy
                .as_ref()
                .and_then(|copy| copy.fetched_at)
                .map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true)),
            "last_error": snapshot.last_error,
            "entries": snapshot
                .copy
                .as_ref()
                .map(|copy| copy.entries.values().map(BTreeMap::len).sum::<usize>())
                .unwrap_or_default(),
        })
    }

    /// The operator's switch: an empty URL turns the feed off and drops its prices, a URL is saved
    /// atomically to `<config_dir>/price-feed.json` — a half-written setting would read as "off"
    /// and silently drop every feed price at the next boot. A URL change drops the old URL's copy —
    /// prices fetched from another feed are not this one's.
    pub(crate) async fn set_url(&self, url: &str) -> anyhow::Result<()> {
        if let Some(dir) = self.setting_file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        crate::util::write_atomic(&self.setting_file, &serde_json::to_vec(&Setting { url: url.to_string() })?).await?;
        let mut snapshot = self.state.read().unwrap().clone();
        let changed = snapshot.url.as_deref() != Some(url);
        snapshot.url = (!url.is_empty()).then(|| url.to_string());
        if changed {
            snapshot.copy = None;
            snapshot.last_error = None;
        }
        *self.state.write().unwrap() = snapshot;
        Ok(())
    }

    /// One sweep: fetch when there is no copy for the current URL or the copy is older than
    /// [`STALE_AFTER`], else nothing. Any failure keeps the last good copy and records
    /// `last_error` — never an empty table, so nothing silently falls back to $0.
    pub(crate) async fn refresh(&self) {
        let previous = self.state.read().unwrap().clone();
        let Some(url) = previous.url.clone() else {
            return;
        };
        let fresh = previous.copy.as_ref().and_then(|copy| copy.fetched_at).is_some_and(|at| {
            Utc::now()
                .signed_duration_since(at)
                .to_std()
                .map(|age| age < STALE_AFTER)
                .unwrap_or(true)
        });
        if fresh {
            return;
        }
        let etag = previous.copy.as_ref().and_then(|copy| copy.etag.clone());
        let answer = fetch(&url, etag.as_deref()).await;
        let (copy, error) = settle(previous.copy.as_deref().cloned(), &url, answer, Utc::now());
        let next = Snapshot {
            url: Some(url.clone()),
            last_error: error,
            copy: copy.map(Arc::new),
        };
        if let Some(copy) = &next.copy
            && let Some(dir) = self.cache_file.parent()
        {
            let _ = std::fs::create_dir_all(dir);
            crate::gateway::write_json_atomic(&self.cache_file, copy);
        }
        // The fetch took a while, and the operator may have saved another URL meanwhile (a PUT spawns
        // a refresh of its own); its answer must not resurrect the superseded one.
        let mut state = self.state.write().unwrap();
        if state.url.as_deref() == Some(url.as_str()) {
            *state = next;
        }
    }
}

/// What one fetch of the feed came back as.
enum Answer {
    NotModified,
    Body { etag: Option<String>, bytes: Vec<u8> },
}

/// The feed's GET with its stored ETag, if there is one. Any failure — unreachable, refused,
/// oversized — is the caller's `Err`, worded for `last_error`.
async fn fetch(url: &str, etag: Option<&str>) -> Result<Answer, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION"), " (+https://colonizer.dev)"))
        .build()
        .map_err(|e| format!("the HTTP client could not be built: {e}"))?;
    let mut request = client.get(url);
    if let Some(etag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let mut response = request
        .send()
        .await
        .map_err(|e| format!("the feed could not be reached: {e}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Answer::NotModified);
    }
    if !status.is_success() {
        return Err(format!("the feed answered {status}"));
    }
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    // Read in chunks so the cap holds while reading: buffering the whole body first would let a
    // runaway URL balloon memory before the check could run.
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("the feed body could not be read: {e}"))?
    {
        if bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(format!("the feed body is over the {MAX_BODY_BYTES} cap"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Answer::Body { etag, bytes })
}

/// The pure half of [`PriceFeed::refresh`], so the keep-the-last-good-copy rule is testable without
/// network: a 200 replaces the copy, a 304 only re-dates it, and any failure leaves the copy for
/// this URL untouched and returns the error instead. A copy for another URL is never kept — its
/// prices are not this feed's.
fn settle(
    previous: Option<Cached>,
    url: &str,
    answer: Result<Answer, String>,
    now: DateTime<Utc>,
) -> (Option<Cached>, Option<String>) {
    let same_url = |copy: &Cached| copy.url == url;
    match answer {
        Ok(Answer::Body { etag, bytes }) => match parse_entries(&bytes) {
            Ok(entries) => (
                Some(Cached {
                    url: url.to_string(),
                    etag,
                    fetched_at: Some(now),
                    entries: Arc::new(entries),
                }),
                None,
            ),
            Err(error) => (previous.filter(same_url), Some(error)),
        },
        Ok(Answer::NotModified) => match previous.filter(same_url) {
            Some(mut copy) => {
                copy.fetched_at = Some(now);
                (Some(copy), None)
            }
            None => (None, Some("the feed answered 304 with no stored copy to reuse".to_string())),
        },
        Err(error) => (previous.filter(same_url), Some(error)),
    }
}

/// The feed body's prices. A bare JSON array is accepted, and so is an object with an `entries` or
/// `prices` array; anything else is a parse error, keeping the last good copy. Malformed entries are
/// skipped, not fatal: one bad row must not unprice every other model.
fn parse_entries(bytes: &[u8]) -> Result<FeedEntries, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| format!("the feed body is not JSON: {e}"))?;
    let list = match value {
        Value::Array(list) => list,
        Value::Object(map) => map
            .get("entries")
            .or_else(|| map.get("prices"))
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "the feed body is an object without an entries array".to_string())?,
        _ => return Err("the feed body is neither an array nor an object with an entries array".to_string()),
    };
    let mut entries: FeedEntries = BTreeMap::new();
    for item in &list {
        if let Some((provider_id, model, entry)) = entry_of(item) {
            entries.entry(provider_id).or_default().insert(model, entry);
        }
    }
    Ok(entries)
}

/// One entry of the feed, or `None` for one the feed must drop: not an object, no provider or
/// model, a rate that is not a number or is negative or non-finite, or neither an input nor an
/// output rate at all. Unknown fields — and unknown envelope keys, like plan window caps — are
/// ignored; a missing rate prices that token kind at nothing.
fn entry_of(item: &Value) -> Option<(String, String, FeedEntry)> {
    let object = item.as_object()?;
    let provider_id = object.get("provider_id")?.as_str()?.trim();
    let model = object.get("model")?.as_str()?.trim();
    if provider_id.is_empty() || model.is_empty() {
        return None;
    }
    // A rate the feed left out is $0; a rate it malforms — a string, a negative, an infinity —
    // spoils the whole entry rather than silently pricing the kind at nothing.
    let rate = |name: &str| -> Option<f64> {
        match object.get(name) {
            None | Some(Value::Null) => Some(0.0),
            Some(Value::Number(number)) => number.as_f64().filter(|rate| rate.is_finite() && *rate >= 0.0),
            Some(_) => None,
        }
    };
    let given = |name: &str| object.get(name).is_some_and(|value| !value.is_null());
    let input_per_mtok = rate("input_per_mtok")?;
    let output_per_mtok = rate("output_per_mtok")?;
    if !given("input_per_mtok") && !given("output_per_mtok") {
        return None;
    }
    Some((
        provider_id.to_string(),
        model.to_string(),
        FeedEntry {
            pricing: Pricing {
                input_per_mtok,
                output_per_mtok,
                cache_read_per_mtok: rate("cache_read_per_mtok")?,
                cache_write_per_mtok: rate("cache_write_per_mtok")?,
                thinking_per_mtok: rate("thinking_per_mtok")?,
            },
            last_verified_at: object
                .get("last_verified_at")
                .and_then(Value::as_str)
                .and_then(verified_string),
            source: object.get("source").and_then(Value::as_str).map(str::to_string),
        },
    ))
}

/// A `last_verified_at` as a moment: the feed may name a bare date (`2026-10-01`) or a full RFC 3339
/// one; `None` for anything else.
fn verified_at(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(date) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return date.and_hms_opt(0, 0, 0).map(|at| at.and_utc());
    }
    DateTime::parse_from_rfc3339(value).ok().map(|at| at.with_timezone(&Utc))
}

/// The same moment, normalized to UTC RFC 3339 for display.
fn verified_string(value: &str) -> Option<String> {
    verified_at(value).map(|at| at.to_rfc3339_opts(SecondsFormat::Secs, true))
}

/// Whether a URL is one the fetcher may be sent to: http or https, and shaped like a URL at all.
fn valid_feed_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    matches!(scheme, "http" | "https") && !rest.is_empty() && !rest.contains(char::is_whitespace)
}

async fn get_feed(State(app): State<Shared>) -> Json<Value> {
    Json(app.price_feed.status())
}

#[derive(Deserialize)]
struct PutFeed {
    url: String,
}

async fn put_feed(State(app): State<Shared>, Json(req): Json<PutFeed>) -> ApiResult<Value> {
    let url = apply(&app, &req.url).await?;
    // A new URL is fetched right away, in the background, so the save never waits on the network;
    // the answer lands in the state the next GET (or boot) reads.
    if !url.is_empty() {
        let app = app.clone();
        tokio::spawn(async move { app.price_feed.refresh().await });
    }
    Ok(Json(app.price_feed.status()))
}

/// The save's handler half minus the fetch: validate the URL, persist it, and return it — shared
/// with the tests, which must not reach the network.
async fn apply(app: &App, url: &str) -> Result<String, AppError> {
    let url = url.trim();
    if !url.is_empty() && !valid_feed_url(url) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "the price feed URL must be an http(s) URL, like https://tokker.dev/v1/feeds/colonizer.json",
        ));
    }
    app.price_feed.set_url(url).await?;
    Ok(url.to_string())
}

pub(crate) fn routes() -> axum::Router<crate::Shared> {
    axum::Router::new().route("/api/price-feed", routing::get(get_feed).put(put_feed))
}

/// The hourly sweep: fetch at start, then check the copy's age once an hour.
pub(crate) fn start_tasks(app: &Shared) {
    let app = app.clone();
    tokio::spawn(async move {
        loop {
            app.price_feed.refresh().await;
            tokio::time::sleep(SWEEP_EVERY).await;
        }
    });
}

/// The price feed as a migrated feature (`features.rs`). Owner-only to scoped API tokens
/// (`token_scope: None`): the feed decides what colonies' work costs, so only the owner may point
/// it. The sweep is the feature's background work.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "price_feed",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: Some(start_tasks),
};

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(input: f64, output: f64) -> FeedEntry {
        FeedEntry {
            pricing: Pricing {
                input_per_mtok: input,
                output_per_mtok: output,
                ..Default::default()
            },
            last_verified_at: None,
            source: None,
        }
    }

    #[test]
    fn a_bare_array_and_both_envelopes_parse_alike() {
        let body = r#"[
            {"provider_id": "deepseek", "model": "deepseek-chat", "input_per_mtok": 0.27, "output_per_mtok": 1.1,
             "last_verified_at": "2026-10-01", "source": "vendor", "plan_window": {"cap": 5}}
        ]"#;
        let bare = parse_entries(body.as_bytes()).unwrap();
        let enveloped = parse_entries(format!(r#"{{"entries": {body}}}"#).as_bytes()).unwrap();
        let prices_envelope = parse_entries(format!(r#"{{"prices": {body}}}"#).as_bytes()).unwrap();
        for parsed in [bare, enveloped, prices_envelope] {
            assert_eq!(
                parsed.get("deepseek").unwrap().get("deepseek-chat").unwrap(),
                &FeedEntry {
                    pricing: Pricing {
                        input_per_mtok: 0.27,
                        output_per_mtok: 1.1,
                        ..Default::default()
                    },
                    last_verified_at: Some("2026-10-01T00:00:00Z".into()),
                    source: Some("vendor".into()),
                }
            );
        }
    }

    #[test]
    fn missing_rates_are_zero_malformed_ones_are_skipped_and_unknown_fields_ignored() {
        let parsed = parse_entries(
            r#"[
            {"provider_id": "a", "model": "kept", "input_per_mtok": 1.0},
            {"provider_id": "a", "model": "nulls-are-absent", "input_per_mtok": 2.0, "cache_read_per_mtok": null, "extra": {"x": 1}},
            {"provider_id": "a", "model": "no-rates"},
            {"provider_id": "a", "model": "zero-and-null", "input_per_mtok": 0},
            {"provider_id": "", "model": "no-provider"},
            {"provider_id": "a", "model": "  ", "input_per_mtok": 1.0},
            {"provider_id": "a", "model": "negative", "input_per_mtok": -0.1, "output_per_mtok": 1.0},
            {"provider_id": "a", "model": "object-rate", "input_per_mtok": {"usd": 1.0}, "output_per_mtok": 1.0},
            {"provider_id": "a", "model": "string-rate", "input_per_mtok": "1.0", "output_per_mtok": 1.0},
            {"provider_id": "b", "model": "other", "output_per_mtok": 3.0, "thinking_per_mtok": 6.0},
            "not-an-object",
            {"provider_id": 4, "model": "typed"}
        ]"#
            .as_bytes(),
        )
        .unwrap();
        let a = parsed.get("a").unwrap();
        assert_eq!(a.get("kept"), Some(&entry(1.0, 0.0)));
        assert_eq!(a.get("nulls-are-absent"), Some(&entry(2.0, 0.0)));
        // An explicitly given 0 is a rate the feed vouched for; absent and null are what is missing.
        assert_eq!(a.get("zero-and-null"), Some(&entry(0.0, 0.0)));
        assert_eq!(a.get("no-rates"), None, "neither an input nor an output rate drops the row");
        assert_eq!(
            parsed.get("b").map(|b| b.get("other").unwrap().pricing.thinking_per_mtok),
            Some(6.0)
        );
        assert_eq!(a.len(), 3, "every malformed row is dropped: {a:?}");
    }

    #[test]
    fn a_body_that_is_not_a_recognized_feed_is_a_parse_error() {
        assert!(parse_entries(br#"{"url": "x"}"#).is_err(), "an object with no entries array");
        assert!(parse_entries(br#""a string""#).is_err());
        assert!(parse_entries(b"not json").is_err());
    }

    #[test]
    fn verified_at_takes_a_date_or_rfc3339_and_normalizes_to_utc() {
        assert_eq!(verified_string("2026-10-01").as_deref(), Some("2026-10-01T00:00:00Z"));
        assert_eq!(
            verified_string("2026-10-01T12:30:00+02:00").as_deref(),
            Some("2026-10-01T10:30:00Z")
        );
        assert_eq!(verified_string("not a date"), None);
    }

    #[test]
    fn stale_is_fourteen_days_past_verification_or_never_verified() {
        let fresh = entry(1.0, 1.0);
        assert!(fresh.stale(), "no verification means stale");
        // Relative to now, so the test does not rot as the hardcoded dates age past the window.
        let at = |days_ago: i64| Some(verified_string(&(Utc::now() - chrono::Duration::days(days_ago)).to_rfc3339()).unwrap());
        let fresh = FeedEntry {
            last_verified_at: at(1),
            ..entry(1.0, 1.0)
        };
        assert!(!fresh.stale(), "verified yesterday");
        let old = FeedEntry {
            last_verified_at: at(20),
            ..entry(1.0, 1.0)
        };
        assert!(old.stale(), "verified over 14 days ago");
    }

    fn stored_copy(url: &str, fetched_at: Option<DateTime<Utc>>) -> Cached {
        Cached {
            url: url.into(),
            etag: Some("\"v1\"".into()),
            fetched_at,
            entries: Arc::new(FeedEntries::from([(
                "deepseek".to_string(),
                BTreeMap::from([("deepseek-chat".to_string(), entry(0.27, 1.1))]),
            )])),
        }
    }

    #[test]
    fn a_failed_or_garbage_fetch_keeps_the_last_good_copy_and_records_the_error() {
        let previous = Some(stored_copy("https://feed", Some(Utc::now())));
        for answer in [
            Err("the feed could not be reached: connection refused".to_string()),
            Err("the feed answered 500 Internal Server Error".to_string()),
            Ok(Answer::Body {
                etag: Some("\"v2\"".into()),
                bytes: b"not json".to_vec(),
            }),
        ] {
            let (copy, error) = settle(previous.clone(), "https://feed", answer, Utc::now());
            assert_eq!(copy.as_ref().map(|c| c.etag.as_deref()), Some(Some("\"v1\"")));
            assert_eq!(copy.unwrap().entries.len(), 1, "the last good copy is untouched");
            assert!(error.is_some(), "the failure is recorded");
        }
    }

    #[test]
    fn a_200_replaces_a_304_only_redates_and_a_mismatched_copy_is_dropped() {
        let now = Utc::now();
        let (copy, error) = settle(
            Some(stored_copy("https://feed", Some(now - chrono::Duration::hours(48)))),
            "https://feed",
            Ok(Answer::NotModified),
            now,
        );
        assert_eq!(error, None);
        let copy = copy.unwrap();
        assert_eq!(copy.etag.as_deref(), Some("\"v1\""), "304 keeps the copy");
        assert_eq!(copy.fetched_at, Some(now), "304 re-dates it");
        assert_eq!(copy.entries.len(), 1);

        let body = Ok(Answer::Body {
            etag: Some("\"v2\"".into()),
            bytes: r#"[{"provider_id": "kimi", "model": "k2", "input_per_mtok": 0.6, "output_per_mtok": 2.5}]"#
                .as_bytes()
                .to_vec(),
        });
        let (copy, error) = settle(None, "https://feed", body, now);
        assert_eq!(error, None);
        let copy = copy.unwrap();
        assert_eq!(copy.etag.as_deref(), Some("\"v2\""));
        assert!(copy.entries.contains_key("kimi"));

        // A copy fetched from another URL is not this feed's, so a failure after a URL change
        // leaves nothing rather than someone else's prices.
        let (copy, error) = settle(
            Some(stored_copy("https://old", Some(now))),
            "https://new",
            Err("the feed could not be reached: no route".into()),
            now,
        );
        assert_eq!(copy, None, "the old URL's prices do not serve the new one");
        assert!(error.is_some());
    }

    #[test]
    fn urls_must_be_http_or_https() {
        assert!(valid_feed_url("https://tokker.dev/v1/feeds/colonizer.json"));
        assert!(valid_feed_url("http://localhost:9000/feed"));
        assert!(!valid_feed_url("ftp://tokker.dev/feed"));
        assert!(!valid_feed_url("file:///etc/passwd"));
        assert!(!valid_feed_url("https://"));
        assert!(!valid_feed_url("https://has space/"));
        assert!(!valid_feed_url("tokker.dev/feed"));
        assert!(!valid_feed_url(""));
    }

    /// The save's rules, through the handler's synchronous half (so no test reaches the network):
    /// a bad scheme is refused with nothing written, a URL is persisted, and `""` turns the feed
    /// off and drops its prices.
    #[tokio::test]
    async fn a_put_turns_the_feed_on_and_off_and_an_invalid_scheme_is_refused() {
        let root = std::env::temp_dir().join(format!("colonizer-price-feed-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        let refused = apply(&app, "ftp://tokker.dev/feed").await.unwrap_err();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert!(
            !app.cfg.config_dir.join("price-feed.json").exists(),
            "the refused save writes nothing"
        );

        let url = apply(&app, " https://tokker.dev/feed ").await.unwrap();
        assert_eq!(url, "https://tokker.dev/feed", "the URL is saved trimmed");
        let status = app.price_feed.status();
        assert_eq!(status["url"], "https://tokker.dev/feed");
        assert_eq!(status["fetched_at"], Value::Null, "nothing fetched yet");
        assert_eq!(
            serde_json::from_slice::<Setting>(&std::fs::read(app.cfg.config_dir.join("price-feed.json")).unwrap())
                .unwrap()
                .url,
            "https://tokker.dev/feed",
            "the setting is persisted"
        );

        // Off again: the URL clears, and no entries of the old feed serve anything.
        let url = apply(&app, "").await.unwrap();
        assert!(url.is_empty());
        let status = app.price_feed.status();
        assert_eq!(status["url"], Value::Null);
        assert_eq!(status["entries"], 0);
        assert_eq!(app.price_feed.entries().len(), 0);
        assert_eq!(
            serde_json::from_slice::<Setting>(&std::fs::read(app.cfg.config_dir.join("price-feed.json")).unwrap())
                .unwrap()
                .url,
            "",
            "the switch is off on disk too"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_copy_for_another_url_is_not_loaded_and_a_stored_copy_survives_a_restart() {
        let root = std::env::temp_dir().join(format!("colonizer-price-feed-{}", crate::util::short_id()));
        let (config_dir, data_dir) = (root.join("config"), root.join("data"));
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(config_dir.join("price-feed.json"), br#"{"url": "https://feed"}"#).unwrap();
        crate::gateway::write_json_atomic(
            &data_dir.join("price-feed-cache.json"),
            &stored_copy("https://other-feed", Some(Utc::now())),
        );
        assert_eq!(
            PriceFeed::load(&config_dir, &data_dir).entries().len(),
            0,
            "another URL's copy is dropped"
        );

        crate::gateway::write_json_atomic(
            &data_dir.join("price-feed-cache.json"),
            &stored_copy("https://feed", Some(Utc::now())),
        );
        let feed = PriceFeed::load(&config_dir, &data_dir);
        assert_eq!(
            feed.entries().get("deepseek").unwrap().get("deepseek-chat"),
            Some(&entry(0.27, 1.1))
        );
        let status = feed.status();
        assert_eq!(status["url"], "https://feed");
        assert_eq!(status["entries"], 1);
        assert_eq!(status["last_error"], Value::Null);
        assert!(status["fetched_at"].is_string());
        let _ = std::fs::remove_dir_all(&root);
    }
}
