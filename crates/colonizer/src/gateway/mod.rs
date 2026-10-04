//! Provider gateway (docs/protocol.md §6.5). Colonies send routed model requests here instead of
//! straight to the provider: the mothership is on the operator's networks (tailnet, LAN), holds the
//! provider keys, and sees every colony, so it can queue requests per provider, apply long timeouts,
//! and report which colonies are waiting on a model. Colonies authenticate with a per-colony token.

use crate::{
    ApiResult, App, Shared, client_error,
    gateway_audit::{GatewayAudit, GatewayFailure},
    openai, orgs, provider_quota,
    providers::{Provider, ProviderQuirks, Usage, Wire, apply_connection_policy, strip_oauth_betas, valid_model},
    util::read_trimmed,
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, post},
};
use chrono::{DateTime, Utc};
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const COLONY_HEADER: &str = "x-colonizer-colony";
/// The token as a plain bearer credential, the form a runner speaking ordinary HTTP auth sends.
pub const BEARER_PREFIX: &str = "Bearer ";
pub const FALLBACK_HEADER: &str = "x-colonizer-fallback";
/// Names a quota-exhausted provider answer (issue #225); the body stays the provider's own.
pub const QUOTA_HEADER: &str = "x-colonizer-quota-exhausted";
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;
/// Large contexts with images can exceed axum's 2 MB default.
const MAX_BODY: usize = 64 * 1024 * 1024;
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a boot-time provider probe result is reused. A dead provider costs each uncached probe
/// up to [`HEALTH_TIMEOUT`], and every boot would pay that again; a minute keeps the `providers`
/// boot warning honest without re-probing an endpoint that was just checked. The manual health
/// check always probes and writes through, so an operator's check also warms the next boot.
pub const PROVIDER_PROBE_TTL: Duration = Duration::from_secs(60);
const FORWARD_HEADERS: [&str; 3] = ["content-type", "accept", "anthropic-version"];
const DROP_RESPONSE_HEADERS: [&str; 6] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "content-length",
];
/// How long an SSE response may go silent before the gateway sends a comment line to keep the
/// connection (and any byte-level idle watchdog downstream) alive during a long prefill.
const SSE_PING_INTERVAL: Duration = Duration::from_secs(15);
const SSE_PING: &[u8] = b": keep-alive\n\n";
/// Longest response body buffered purely to count a non-streaming usage. Past this the response still
/// forwards whole; its tokens just aren't priced.
const MAX_TAP_BODY: usize = 4 * 1024 * 1024;

/// How long dirty usage counters may go unwritten; a crash loses at most this much of the tally.
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Cumulative per-provider usage, kept across restarts in `<data_dir>/provider-usage.json`. The live
/// `in_flight`/`queued` gauges are zero whenever nobody is mid-request, so these counters are what says
/// whether a request has ever actually gone to the provider.
///
/// - `requests`: requests the gateway accepted for this provider. Counted once everything that can refuse a
///   request locally has passed (colony auth, provider lookup, path and body translation), so queueing, the
///   upstream call and the streamed body are all included, and a request the gateway itself refuses is not.
///   An attempt that queues past `queue_timeout_secs` and never reaches the provider still counts — it was a
///   real request against this provider — so `requests` is not a count of requests the provider saw.
/// - `failures`: requests that produced no usable upstream response — one of the gateway's three fallback
///   answers (queue timeout, unreachable, timeout), an upstream status >= 400, or an openai-wire response
///   whose body failed or never finished. A failure after the headers, part-way through a streamed body,
///   is not counted. A subset of `requests`.
/// - `fallbacks`: requests that will fall back to Claude. A prediction, not an observation: the gateway
///   answered 502/503/504 with `x-colonizer-fallback` and the provider has a `fallback_model`, which is
///   exactly when the colony's model router (router.mjs) retries on Claude. The retry itself never comes
///   back through the gateway. A subset of `failures`.
/// - `duration_ms`: cumulative wall-clock time of dispatched requests, including streaming the response
///   body. Timed from when a request's concurrency slot was acquired, so time spent queued is never counted.
/// - `last_request_at`: when the last request was accepted (before any queue wait), RFC3339 like the other
///   timestamps here.
/// - `since`: when the tally for this provider started — the first counted request, RFC3339 like the other
///   timestamps here. Absent for a tally with no requests yet or one kept by an older build.
/// - `last_failure`: the code of the most recent failure, one of [`gateway_audit::GatewayFailure`]'s;
///   `null` while nothing has failed. Served once, under `health`, where the cockpit and the
///   notifications read it; the tally itself starts a restart without it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderUsage {
    pub requests: u64,
    pub failures: u64,
    pub fallbacks: u64,
    pub duration_ms: u64,
    pub last_request_at: Option<DateTime<Utc>>,
    pub since: Option<DateTime<Utc>>,
    #[serde(skip_serializing)]
    pub last_failure: Option<String>,
}

/// Enough requests to judge a provider by its failure rate.
pub const HEALTH_MIN_SAMPLE: u64 = 50;
/// At or above this percentage of failed requests a provider is degraded.
pub const DEGRADED_PCT: f64 = 10.0;

/// What [`ProviderUsage`] says about a provider at a glance: its failure rate and latency, and whether
/// there is enough data to judge it. One rule for every surface — the providers API, `/api/status` and
/// notifications — so a provider the fan-out is drowning looks the same everywhere.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageHealth {
    /// `failures/requests * 100`, rounded to one decimal place; `0.0` when there are no requests.
    pub failure_pct: f64,
    /// `duration_ms/requests`; `0` when there are no requests.
    pub avg_latency_ms: u64,
    /// `requests >= HEALTH_MIN_SAMPLE` — enough data to judge by failure rate.
    pub rated: bool,
    /// `rated && failure_pct >= DEGRADED_PCT`. An unrated provider is never degraded: a handful of
    /// early failures is noise, and the fan-out this reports on starts at tens of requests.
    pub degraded: bool,
    /// The code of the most recent failure (`gateway_audit::GatewayFailure`), or `null`.
    pub last_failure: Option<String>,
}

/// Rates a provider's cumulative usage by the one rule every surface shares. Pure: same usage in, same
/// verdict out, whatever calls it.
pub fn health(usage: &ProviderUsage) -> UsageHealth {
    let failure_pct = if usage.requests == 0 {
        0.0
    } else {
        (usage.failures as f64 / usage.requests as f64 * 1000.0).round() / 10.0
    };
    UsageHealth {
        failure_pct,
        // Zero requests took zero time per request; `checked_div` says so without a divide-by-zero.
        avg_latency_ms: usage.duration_ms.checked_div(usage.requests).unwrap_or_default(),
        rated: usage.requests >= HEALTH_MIN_SAMPLE,
        degraded: usage.requests >= HEALTH_MIN_SAMPLE && failure_pct >= DEGRADED_PCT,
        last_failure: usage.last_failure.clone(),
    }
}

/// Counts up while alive; used for in-flight and queued requests.
struct Counted(Arc<AtomicU64>);

impl Counted {
    fn new(counter: &Arc<AtomicU64>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter.clone())
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How many of one colony's requests may wait for a provider slot at once. Claude Code works
/// through parallel subagents, so bursts well past the provider's own concurrency are normal;
/// past this a colony's next request is refused instead of parked, and its client retries it.
pub const COLONY_QUEUE_CAP: u64 = 16;

/// Claims one of `cap` places on `counter` in a compare-and-swap, so parallel requests cannot all
/// read the same total and slip past the cap together. `None` when the cap held: the caller
/// refuses without queuing.
fn counted_within(counter: &Arc<AtomicU64>, cap: u64) -> Option<Counted> {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        if current >= cap {
            return None;
        }
        match counter.compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Some(Counted(counter.clone())),
            Err(seen) => current = seen,
        }
    }
}

/// Gives back what it took when dropped: one request's reservation of estimated spend against the
/// colony's budget, added on dispatch (see [`proxy`]) and subtracted only once the response's real
/// cost has been recorded — the recorder's task holds it across the gap between "body streamed"
/// and "cost landed" that `enforce_budget` cannot see into (issue #409).
struct Reserved(Arc<AtomicU64>, u64);

impl Reserved {
    /// An unconditional claim, for tests that set up an outstanding total; requests use [`Reserved::try_new`].
    #[cfg(test)]
    fn new(reserved: &Arc<AtomicU64>, micro_usd: u64) -> Self {
        reserved.fetch_add(micro_usd, Ordering::SeqCst);
        Self(reserved.clone(), micro_usd)
    }

    /// Adds `micro_usd` to `reserved` only while `fits(outstanding)` holds for the value it adds to,
    /// in one compare-and-swap: two parallel requests cannot both read the same outstanding total and
    /// both pass. `Err(outstanding)` is the total that did not fit.
    fn try_new(reserved: &Arc<AtomicU64>, micro_usd: u64, fits: impl Fn(u64) -> bool) -> Result<Self, u64> {
        let mut current = reserved.load(Ordering::SeqCst);
        loop {
            if !fits(current) {
                return Err(current);
            }
            match reserved.compare_exchange(current, current + micro_usd, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return Ok(Self(reserved.clone(), micro_usd)),
                Err(seen) => current = seen,
            }
        }
    }
}

impl Drop for Reserved {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Ordering::SeqCst);
    }
}

/// Adds the elapsed wall-clock time to the provider's cumulative `duration_ms` when dropped — the same
/// lifetime as the request's other guards, so the streamed body is included. Created once the request's
/// concurrency slot is acquired, not while it waits for one, so `duration_ms` measures dispatched time only.
struct Timed {
    counters: Arc<UsageCounters>,
    start: Instant,
}

impl Timed {
    fn new(counters: Arc<UsageCounters>) -> Self {
        Self {
            counters,
            start: Instant::now(),
        }
    }
}

impl Drop for Timed {
    fn drop(&mut self) {
        self.counters
            .duration_ms
            .fetch_add(self.start.elapsed().as_millis() as u64, Ordering::SeqCst);
        self.counters.dirty.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct ProviderStats {
    in_flight: Arc<AtomicU64>,
    queued: Arc<AtomicU64>,
}

/// Live cumulative usage for one provider, seeded from disk at startup and written back by
/// `flush_usage` when dirty. The request path only touches these atomics, never the file.
#[derive(Default)]
struct UsageCounters {
    requests: AtomicU64,
    failures: AtomicU64,
    fallbacks: AtomicU64,
    duration_ms: AtomicU64,
    last_request_at: Mutex<Option<DateTime<Utc>>>,
    since: Mutex<Option<DateTime<Utc>>>,
    /// The most recent failure's code, kept with the counts it belongs to.
    last_failure: Mutex<Option<String>>,
    /// Set by every change; `flush_usage` clears it and writes.
    dirty: AtomicBool,
}

impl UsageCounters {
    fn seeded(usage: ProviderUsage) -> Self {
        Self {
            requests: AtomicU64::new(usage.requests),
            failures: AtomicU64::new(usage.failures),
            fallbacks: AtomicU64::new(usage.fallbacks),
            duration_ms: AtomicU64::new(usage.duration_ms),
            last_request_at: Mutex::new(usage.last_request_at),
            since: Mutex::new(usage.since),
            last_failure: Mutex::new(usage.last_failure),
            dirty: AtomicBool::new(false),
        }
    }

    fn add_request(&self) {
        // The tally's start instant is set once, on the first counted request, and never moves. It is
        // stamped before the counter increments, so a snapshot landing in between never persists
        // `requests >= 1` with a `since` of `None` — a restart would then re-stamp the tally later
        // than the truth.
        {
            let mut since = self.since.lock().unwrap();
            if since.is_none() {
                *since = Some(Utc::now());
            }
        }
        self.requests.fetch_add(1, Ordering::SeqCst);
        *self.last_request_at.lock().unwrap() = Some(Utc::now());
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// A failure with no fallback answer: an upstream status >= 400, or an openai-wire body that failed or
    /// never finished. The colony's router retries none of these — only the gateway's own three fallback
    /// errors get that. `failure` names the branch, kept as the provider's `last_failure`.
    fn add_failure(&self, failure: GatewayFailure) {
        *self.last_failure.lock().unwrap() = Some(failure.code().to_string());
        self.failures.fetch_add(1, Ordering::SeqCst);
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// One of the three gateway-level fallback errors, and — when the provider has a fallback model —
    /// the fallback to Claude the colony's router will make with it (see [`ProviderUsage::fallbacks`]).
    fn add_failure_with_fallback(&self, provider: &Provider, failure: GatewayFailure) {
        self.add_failure(failure);
        // The router retries the transport failures on a Claude fallback only; a provider-prefixed
        // fallback is the gateway's own retry, which quota exhaustion alone triggers (issue #767).
        let retried = provider.claude_fallback().is_some()
            || (failure == GatewayFailure::QuotaExhausted && provider.fallback_model.as_deref().is_some_and(|m| !m.is_empty()));
        if retried {
            self.fallbacks.fetch_add(1, Ordering::SeqCst);
            self.dirty.store(true, Ordering::SeqCst);
        }
    }

    fn snapshot(&self) -> ProviderUsage {
        ProviderUsage {
            requests: self.requests.load(Ordering::SeqCst),
            failures: self.failures.load(Ordering::SeqCst),
            fallbacks: self.fallbacks.load(Ordering::SeqCst),
            duration_ms: self.duration_ms.load(Ordering::SeqCst),
            last_request_at: *self.last_request_at.lock().unwrap(),
            since: *self.since.lock().unwrap(),
            last_failure: self.last_failure.lock().unwrap().clone(),
        }
    }
}

struct Limit {
    max: u64,
    slots: Arc<Semaphore>,
}

/// Writes `value` to `path` atomically (tmp + rename). The tmp path is unique per call: writers sharing
/// one path interleave their writes and can rename a half-overwritten file into place, which
/// `Gateway::new` would read as corrupt and silently reset.
fn write_json_atomic(path: &std::path::Path, value: &impl Serialize) {
    if let Ok(data) = serde_json::to_vec_pretty(value) {
        let tmp = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4()));
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        } else {
            // A failed write may have left a partial tmp behind; it must not pile up in the data dir.
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// One provider's quota-exhaustion record: when the plan refills, and when it ran out. Kept across
/// restarts in `<data_dir>/provider-quota.json`: a restart that forgot it would resume every
/// quota-parked colony at once, only for each to hit the quota again and re-park.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuotaState {
    pub reset_at: Option<String>,
    pub reset_unix: Option<i64>,
    pub since: DateTime<Utc>,
}

/// The quota-store id for the Claude account's own cap (session/usage/weekly limits name no routed
/// provider). A dedicated record — never a real provider's — so healthy providers stay healthy
/// while the account pause holds. Persisted in `provider-quota.json` with the rest, so it survives
/// a restart like any provider record.
pub const ACCOUNT_QUOTA_ID: &str = "claude-account";

/// A colony whose requests to one provider keep coming back quota-exhausted with no Claude
/// fallback offered (issues #760, #767): the provider, how many such answers in a row, and when the
/// first one landed. In memory only — the provider's own record in `provider-quota.json` is what
/// survives a restart; this only ties colonies to it. Any upstream success for the colony clears it.
#[derive(Clone, Debug, PartialEq)]
pub struct ColonyQuotaHit {
    pub provider: String,
    pub hits: u64,
    pub since: DateTime<Utc>,
}

pub struct Gateway {
    client: reqwest::Client,
    stats: Mutex<HashMap<String, Arc<ProviderStats>>>,
    limits: Mutex<HashMap<String, Limit>>,
    /// Requests each colony has open through the gateway, queued or streaming.
    colonies: Mutex<HashMap<String, Arc<AtomicU64>>>,
    /// Requests each colony has waiting for a provider slot, capped at [`COLONY_QUEUE_CAP`].
    colony_queue: Mutex<HashMap<String, Arc<AtomicU64>>>,
    /// Estimated spend each colony has in flight but not yet recorded, in micro-dollars (`usd * 1e6`
    /// rounded) so a running dollar total fits an atomic: the per-request reservations that close the
    /// gap between dispatch and `record_routed_usage`, without which parallel requests each pass the
    /// budget check before any of them records its cost (issue #409).
    reserved: Mutex<HashMap<String, Arc<AtomicU64>>>,
    /// Cumulative usage per provider, seeded from `usage_file` at startup and written back to it when dirty.
    usage: Mutex<HashMap<String, Arc<UsageCounters>>>,
    usage_file: PathBuf,
    /// Quota-exhausted providers by id; entries with a passed `reset_unix` read as recovered.
    quota: Mutex<HashMap<String, QuotaState>>,
    quota_file: PathBuf,
    /// Colonies blocked on an exhausted provider, by colony id (see [`ColonyQuotaHit`]).
    colony_quota: Mutex<HashMap<String, ColonyQuotaHit>>,
}

impl Gateway {
    pub fn new(data_dir: &std::path::Path) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let usage_file = data_dir.join("provider-usage.json");
        // A missing or corrupt file means the counters start over, never that the gateway fails.
        let saved: BTreeMap<String, ProviderUsage> = std::fs::read(&usage_file)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        let usage: HashMap<String, Arc<UsageCounters>> = saved
            .into_iter()
            .map(|(id, usage)| (id, Arc::new(UsageCounters::seeded(usage))))
            .collect();
        let quota_file = data_dir.join("provider-quota.json");
        // Likewise a missing or corrupt quota file means no provider is out, and the next quota error
        // re-learns it. Lapsed records are dropped here so they never outlive the restart that finds them.
        let now = Utc::now();
        let quota: HashMap<String, QuotaState> = std::fs::read(&quota_file)
            .ok()
            .and_then(|data| serde_json::from_slice::<HashMap<String, QuotaState>>(&data).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, q)| provider_quota::quota_active(q.reset_unix, q.since, now))
            .collect();
        Ok(Self {
            client,
            stats: Default::default(),
            limits: Default::default(),
            colonies: Default::default(),
            colony_queue: Default::default(),
            reserved: Default::default(),
            usage: Mutex::new(usage),
            usage_file,
            quota: Mutex::new(quota),
            quota_file,
            colony_quota: Default::default(),
        })
    }

    fn stats(&self, provider: &str) -> Arc<ProviderStats> {
        self.stats.lock().unwrap().entry(provider.to_string()).or_default().clone()
    }

    /// `(in_flight, queued)` for a provider across all colonies.
    pub fn load(&self, provider: &str) -> (u64, u64) {
        let stats = self.stats(provider);
        (stats.in_flight.load(Ordering::SeqCst), stats.queued.load(Ordering::SeqCst))
    }

    fn usage_counters(&self, provider: &str) -> Arc<UsageCounters> {
        self.usage.lock().unwrap().entry(provider.to_string()).or_default().clone()
    }

    /// Cumulative usage for a provider since the counters were first kept.
    pub fn usage(&self, provider: &str) -> ProviderUsage {
        self.usage_counters(provider).snapshot()
    }

    /// Writes the whole usage map atomically (see [`write_json_atomic`]). Unlike the crate's other JSON
    /// state this file has three concurrent writers — the flush loop, the shutdown flush and
    /// [`Self::forget_usage`]. Renames can land out of order, but each one is a complete snapshot, so the
    /// worst a lost race does is persist a slightly stale tally until the next flush.
    fn write_usage(&self) {
        let snapshot: BTreeMap<String, ProviderUsage> = self
            .usage
            .lock()
            .unwrap()
            .iter()
            .map(|(id, counters)| (id.clone(), counters.snapshot()))
            .collect();
        write_json_atomic(&self.usage_file, &snapshot);
    }

    /// Writes the quota map while its lock is held. Unlike the usage tally there is no later flush to
    /// repair a stale snapshot, so writers are serialized: a mark renamed over a newer clear would
    /// re-park colonies after the next restart.
    fn write_quota(&self, quota: &HashMap<String, QuotaState>) {
        if let Some(dir) = self.quota_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let snapshot: BTreeMap<&String, &QuotaState> = quota.iter().collect();
        write_json_atomic(&self.quota_file, &snapshot);
    }

    /// Writes the usage counters if anything changed since the last flush. Called every
    /// [`USAGE_FLUSH_INTERVAL`] and on shutdown, never per request: the request path only touches the
    /// in-memory counters, so a crash loses at most [`USAGE_FLUSH_INTERVAL`] of the tally.
    pub fn flush_usage(&self) {
        // Swaps every flag: `any` would stop at the first dirty entry and leave the rest set even though
        // write_usage below persists the whole map.
        let mut dirty = false;
        {
            let map = self.usage.lock().unwrap();
            for counters in map.values() {
                dirty |= counters.dirty.swap(false, Ordering::SeqCst);
            }
        }
        if !dirty {
            return;
        }
        if let Some(dir) = self.usage_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        self.write_usage();
    }

    /// Forgets a provider's usage and flushes at once, so deleting the provider also deletes its tally
    /// even if nothing else is dirty.
    pub fn forget_usage(&self, provider: &str) {
        if self.usage.lock().unwrap().remove(provider).is_some() {
            if let Some(dir) = self.usage_file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            self.write_usage();
        }
    }

    /// Records a provider's plan as exhausted, with the reset the error named, if any.
    pub fn mark_quota_exhausted(&self, provider: &str, reset_at: Option<String>, reset_unix: Option<i64>) {
        let mut quota = self.quota.lock().unwrap();
        quota.insert(
            provider.to_string(),
            QuotaState {
                reset_at,
                reset_unix,
                since: Utc::now(),
            },
        );
        self.write_quota(&quota);
    }

    /// Records the Claude account's own cap as exhausted, with the reset the error named, if any.
    /// Marks no real provider: the queue pauses on this record alone, and a routed success on any
    /// provider leaves it held — only its lapse or an explicit account resume clears it.
    pub fn mark_account_quota_exhausted(&self, reset_at: Option<String>, reset_unix: Option<i64>) {
        self.mark_quota_exhausted(ACCOUNT_QUOTA_ID, reset_at, reset_unix);
    }

    /// The account's quota record, if it has one — expired or not;
    /// [`Self::is_account_quota_exhausted`] judges.
    pub fn account_quota_state(&self) -> Option<QuotaState> {
        self.quota_state(ACCOUNT_QUOTA_ID)
    }

    /// True while the Claude account's cap holds: recorded and still active — a named reset ahead,
    /// or a reset-less mark younger than its TTL.
    pub fn is_account_quota_exhausted(&self) -> bool {
        self.is_quota_exhausted(ACCOUNT_QUOTA_ID)
    }

    /// Forgets the account's quota record: the explicit account resume. A routed success never
    /// lands here (see [`Self::clear_quota_on_success`).
    pub fn forget_account_quota(&self) {
        self.forget_quota(ACCOUNT_QUOTA_ID);
    }
    /// The provider's quota record, if it has one — expired or not; [`Self::is_quota_exhausted`] judges.
    pub fn quota_state(&self, provider: &str) -> Option<QuotaState> {
        self.quota.lock().unwrap().get(provider).cloned()
    }

    /// True while the provider's plan is out: recorded and still active — a named reset ahead, or
    /// a reset-less mark younger than its TTL. A lapsed record reads as recovered without a re-probe.
    pub fn is_quota_exhausted(&self, provider: &str) -> bool {
        let now = Utc::now();
        self.quota
            .lock()
            .unwrap()
            .get(provider)
            .is_some_and(|q| provider_quota::quota_active(q.reset_unix, q.since, now))
    }

    /// Every still-exhausted provider with its reset, by id — the account record under
    /// [`ACCOUNT_QUOTA_ID`] included while it holds, so an unnamed account-parked colony stays
    /// parked on it and resumes when it lapses. What the status poll and the queue read.
    pub fn quota_exhausted(&self) -> Vec<(String, Option<String>, Option<i64>)> {
        let now = Utc::now();
        let mut out: Vec<_> = self
            .quota
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, q)| provider_quota::quota_active(q.reset_unix, q.since, now))
            .map(|(id, q)| (id.clone(), q.reset_at.clone(), q.reset_unix))
            .collect();
        out.sort();
        out
    }

    /// An upstream 2xx for the provider proves the plan is back: forget its quota record, so the
    /// queue unpauses and parked colonies resume on the next tick. Never the account record: one
    /// routed success says nothing about the Claude account's own cap, which lifts only on its lapse
    /// or an explicit account resume ([`Self::forget_account_quota`]).
    pub fn clear_quota_on_success(&self, provider: &str) {
        if provider == ACCOUNT_QUOTA_ID {
            return;
        }
        self.forget_quota(provider);
    }

    /// Forgets a provider's quota record with its usage, so a deleted provider recovers by removal.
    /// Writes only when a record went: every upstream 2xx lands here, and most have nothing to clear.
    pub fn forget_quota(&self, provider: &str) {
        let mut quota = self.quota.lock().unwrap();
        if quota.remove(provider).is_some() {
            self.write_quota(&quota);
        }
    }

    /// Notes that `colony`'s request to `provider` came back quota-exhausted with no fallback: the
    /// colony is blocked on that provider until a request of its own succeeds. A hit on another
    /// provider replaces the record — the colony is blocked on whichever answered last.
    pub fn note_colony_quota(&self, colony: &str, provider: &str) {
        let mut map = self.colony_quota.lock().unwrap();
        match map.get_mut(colony) {
            Some(hit) if hit.provider == provider => hit.hits += 1,
            _ => {
                map.insert(
                    colony.to_string(),
                    ColonyQuotaHit {
                        provider: provider.to_string(),
                        hits: 1,
                        since: Utc::now(),
                    },
                );
            }
        }
    }

    /// Forgets a colony's quota block: one of its requests succeeded, or it was switched, stopped
    /// or parked by a quota action.
    pub fn clear_colony_quota(&self, colony: &str) {
        self.colony_quota.lock().unwrap().remove(colony);
    }

    /// The provider `colony` is blocked on, if every request it made since its last success came
    /// back quota-exhausted.
    pub fn colony_quota(&self, colony: &str) -> Option<ColonyQuotaHit> {
        self.colony_quota.lock().unwrap().get(colony).cloned()
    }

    /// Every blocked colony, by id.
    pub fn colony_quota_all(&self) -> HashMap<String, ColonyQuotaHit> {
        self.colony_quota.lock().unwrap().clone()
    }

    /// `COLONIZER_QUOTA_FALLBACK=0` (or `false`) opts every role out of quota failover at once;
    /// anything else — including unset — keeps it on. Checked where the fallback is offered.
    pub fn quota_fallback_enabled() -> bool {
        std::env::var("COLONIZER_QUOTA_FALLBACK")
            .ok()
            .is_none_or(|v| !matches!(v.as_str(), "0" | "false"))
    }

    /// The colony's in-flight request counter — the one [`colony_busy`](Self::colony_busy) reads.
    /// `pub(crate)` so a test can hold a colony busy directly (watchdog.rs, issue #878).
    pub(crate) fn colony_counter(&self, colony: &str) -> Arc<AtomicU64> {
        self.colonies.lock().unwrap().entry(colony.to_string()).or_default().clone()
    }

    /// The colony's requests waiting for a provider slot (see [`COLONY_QUEUE_CAP`]).
    fn colony_queue_counter(&self, colony: &str) -> Arc<AtomicU64> {
        self.colony_queue
            .lock()
            .unwrap()
            .entry(colony.to_string())
            .or_default()
            .clone()
    }

    /// The colony's outstanding spend reservations, in micro-dollars (see [`Gateway::reserved`]).
    fn colony_reserved(&self, colony: &str) -> Arc<AtomicU64> {
        self.reserved.lock().unwrap().entry(colony.to_string()).or_default().clone()
    }

    /// True while the colony is waiting on a model through the gateway; the watchdog counts that as progress.
    pub fn colony_busy(&self, colony: &str) -> bool {
        self.colonies
            .lock()
            .unwrap()
            .get(colony)
            .is_some_and(|c| c.load(Ordering::SeqCst) > 0)
    }

    /// The provider's request slots, or `None` when it has no concurrency limit. A changed limit gets a
    /// fresh semaphore; requests holding the old one finish without counting against the new limit.
    fn slots(&self, provider: &str, max: Option<u64>) -> Option<Arc<Semaphore>> {
        let mut limits = self.limits.lock().unwrap();
        let Some(max) = max else {
            limits.remove(provider);
            return None;
        };
        let limit = limits.entry(provider.to_string()).or_insert_with(|| Limit {
            max,
            slots: Arc::new(Semaphore::new(max as usize)),
        });
        if limit.max != max {
            *limit = Limit {
                max,
                slots: Arc::new(Semaphore::new(max as usize)),
            };
        }
        Some(limit.slots.clone())
    }
}

impl App {
    pub fn gateway_token_file(&self, session: &str) -> std::path::PathBuf {
        self.session_dir(session).join("gateway-token")
    }

    /// The live colony a gateway token belongs to, record and all: `proxy` needs the session itself,
    /// not just the id, to check which providers the colony may spend on.
    pub(crate) async fn colony_for_token(&self, token: &str) -> Option<crate::sessions::Session> {
        if token.len() < 32 {
            return None;
        }
        let sessions = self.sessions.read().await;
        sessions
            .iter()
            .filter(|s| s.status.is_live())
            .find(|s| {
                read_trimmed(&self.gateway_token_file(&s.id)).is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes()))
            })
            .cloned()
    }
}

/// Compares two secrets without short-circuiting, so a wrong guess costs the same regardless of
/// where it differs. Lengths are not hidden (every token here has a fixed length).
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/providers/{id}/{*path}", any(proxy))
        .route("/recall", post(recall))
        .route("/history", post(crate::history::history))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}

/// A colony's deja recall request (issue #495): the query, and at most how many hits back.
#[derive(Deserialize)]
struct RecallBody {
    query: String,
    limit: Option<u32>,
}

/// `POST /recall` on the colony gateway: read-only deja recall over the *token's own org's* index.
/// Authenticated like every gateway route with the colony's per-colony bearer token; a token that
/// does not name a live colony is a 401, and an org with no index — deja off, never indexed — gets
/// empty hits rather than an error, the same answer an empty index would give.
async fn recall(State(app): State<Shared>, headers: HeaderMap, Json(body): Json<RecallBody>) -> Response {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let Some(s) = app.colony_for_token(token).await else {
        return api_error(StatusCode::UNAUTHORIZED, "authentication_error", "unknown colony token", None);
    };
    match crate::deja::search(&app, &s.org, &body.query, body.limit).await {
        Ok(hits) => Json(hits).into_response(),
        Err(e) => api_error(StatusCode::BAD_GATEWAY, "api_error", format!("deja: {e:#}"), None),
    }
}

/// Flushes dirty usage counters every [`USAGE_FLUSH_INTERVAL`]; spawned once at startup.
pub async fn flush_loop(app: Shared) {
    loop {
        tokio::time::sleep(USAGE_FLUSH_INTERVAL).await;
        app.gateway.flush_usage();
    }
}

/// An error in Anthropic's shape, so Claude Code reports it like any API error.
pub(crate) fn api_error(status: StatusCode, kind: &str, message: impl Into<String>, fallback: Option<&'static str>) -> Response {
    let mut response = (
        status,
        Json(json!({"type": "error", "error": {"type": kind, "message": message.into()}})),
    )
        .into_response();
    if let Some(reason) = fallback {
        response
            .headers_mut()
            .insert(FALLBACK_HEADER, HeaderValue::from_static(reason));
    }
    response
}

mod probe;
mod proxy;
mod stream;
#[cfg(test)]
mod tests;

use self::{proxy::*, stream::*};

// Re-exported so the `crate::gateway::X` paths the rest of the crate calls keep resolving: the probe
// surface (boot, providers, server) whole, plus the two single items the other children export.
pub(crate) use self::probe::*;
pub(crate) use self::{proxy::MODEL_ERROR_REASON, stream::credential_header};
