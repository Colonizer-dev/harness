//! The exporter task (#849): one loop that tails the ledgers, maps their lines, sends the records
//! over OTLP/HTTP and commits the cursors only once the backend has acknowledged them.
//!
//! - **The files are the spool.** A failed request is retried from the outbox it was built into;
//!   nothing new is read while it waits, and nothing is committed, so a crash replays the same
//!   lines (record ids are deterministic, so a backend that dedupes absorbs the replay).
//! - **Backoff** is 1 s doubling to 60 s, with jitter, reset by the next success.
//! - **Bisect:** a request the backend refuses for its records (400, 413, 422) is split in half and
//!   each half retried, down to one record, which is then dropped and counted rather than retried
//!   forever. Nothing else is ever dropped by the transport.
//! - **A bad credential drops nothing:** 401, 403 and 407 hold the batch and its cursors, set the
//!   health to `auth_failed`, and retry with backoff until the key works (a fixed key reaches a
//!   running add-on as a restart, which replays from the committed cursors).
//! - **`Retry-After`** on a 429 or 503 is honoured (seconds or an HTTP-date, capped at 5 minutes).
//! - **Bounded memory:** each tick reads at most `max_read_mib_per_sec` worth of lines (and at most
//!   [`crate::tailer::MAX_ITEMS_PER_TICK`] records), shared fairly among the sources by the [`crate::tailer`], so
//!   the outbox is never more than one tick's batch. During an outage the backlog stays on disk; a
//!   line older than `max_backlog_days` is dropped (the oldest data first), counted in
//!   `colonizer.observability.dropped{reason="backlog"}`, and reported by one `export_gap` record.
//! - **Where a new destination starts:** `start_from = now` (the default) binds every file that
//!   exists to its end the first time a destination is seen, so nothing historical is sent;
//!   `backlog` reads every file from its start, within `max_backlog_days`.
//! - **Never in a colony's way:** this is a separate process the mothership supervises; nothing in a
//!   colony's path waits on it.

use crate::batch::{BatchConfig, Batcher, ExportResource};
use crate::contract::{self, Contract};
use crate::cursor::Cursor;
use crate::encode::Request;
use crate::hashing::HashKey;
pub use crate::health::Status;
use crate::health::{PartialSuccess, backlog_bytes};
use crate::map::Mapper;
use crate::metrics::Aggregates;
use crate::policy::{AttrValue, ContentGate, Policy, PolicyConfig, RepoNames};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
use crate::sources;
use crate::state::{CursorKey, Signal, State};
use crate::tailer::{Inputs, Tailer};
use crate::transport::{Outcome, Transport};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The pause between ticks with nothing to retry.
pub const TICK: Duration = Duration::from_secs(1);
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// The key the metric series are committed under in `state.json`'s `extra`.
const METRICS_KEY: &str = "metrics";
/// The key, in `state.json`'s `extra`, of the destinations whose start position is settled.
const DESTINATIONS_KEY: &str = "destinations";

pub fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Requests built but not yet acknowledged, and what to commit once they are.
struct Outbox {
    requests: Vec<Request>,
    records: u64,
    cursors: Vec<(CursorKey, Cursor)>,
    /// Read positions of deleted colonies, dropped on commit.
    removed: Vec<CursorKey>,
    /// Colonies found deleted, recorded on commit so they are never stat'ed again.
    gone: Vec<String>,
    aggregates: Aggregates,
}

/// Why a flush stopped short: the outbox is kept for the next try.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// Retry after a backoff, or after the server's `Retry-After` when it gave one.
    Retry(Option<Duration>),
    /// The credential was refused.
    Auth,
}

fn unix_now() -> u64 {
    now_nanos() / 1_000_000_000
}

pub struct Exporter {
    contract: Contract,
    contract_path: PathBuf,
    contract_mtime: Option<SystemTime>,
    data_dir: PathBuf,
    policy: Policy,
    resource: ExportResource,
    transport: Transport,
    state: State,
    aggregates: Aggregates,
    destination: String,
    outbox: Option<Outbox>,
    backoff: Option<(Instant, Duration)>,
    last_metrics: Option<Instant>,
    /// The multi-source reader: fairness, rate limit, drained and deleted colonies.
    tailer: Tailer,
    /// The `export_gap` records of the last read, as the tailer produced them.
    pub(crate) last_gaps: Vec<serde_json::Value>,
    pub status: Status,
    last_status_write: Option<Instant>,
}

/// The policy, built from the contract's settings: content gate closed (#848 has not landed), the
/// hash key loaded for `repo_names = hashed` (without one, hashed names are dropped, never sent).
pub fn policy_for(contract: &Contract) -> Policy {
    let s = &contract.settings;
    let repo_names = if s.repo_names == "hashed" {
        RepoNames::Hashed
    } else {
        RepoNames::Plain
    };
    let key = match repo_names {
        RepoNames::Hashed => HashKey::load_or_create(&contract.data_dir).ok(),
        RepoNames::Plain => None,
    };
    let config = PolicyConfig {
        max_attribute_bytes: s.max_attribute_bytes as usize,
        max_content_bytes: s.max_content_bytes as usize,
        repo_names,
    };
    Policy::new(config, ContentGate::closed(), key)
}

/// The resource every request carries: `service.name`, `service.version`, `service.instance.id`
/// (the host id), `colonizer.fleet.id`, and `OTEL_RESOURCE_ATTRIBUTES` as the mothership passed it.
pub fn resource_for(policy: &Policy, contract: &Contract) -> ExportResource {
    let s = &contract.settings;
    let service = if s.service_name.is_empty() {
        "colonizer"
    } else {
        s.service_name.as_str()
    };
    let fleet = if contract.fleet_id.is_empty() {
        &contract.host_id
    } else {
        &contract.fleet_id
    };
    let mut attrs: Vec<(String, AttrValue)> = s
        .resource_attributes
        .iter()
        .map(|(k, v)| (k.clone(), AttrValue::Str(v.clone())))
        .collect();
    let mut set = |k: &str, v: &str| {
        attrs.retain(|(key, _)| key != k);
        attrs.push((k.to_string(), AttrValue::Str(v.to_string())));
    };
    set("service.name", service);
    set("service.version", &contract.mothership_version);
    set("service.instance.id", &contract.host_id);
    set("colonizer.fleet.id", fleet);
    let refs: Vec<(&str, AttrValue)> = attrs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    policy.resource(&refs)
}

/// The destination a cursor belongs to: a new endpoint starts its own cursors (from the backlog
/// limit), rather than skipping what the old one never received.
fn destination_hash(contract: &Contract) -> String {
    let s = &contract.settings;
    let key = format!("{}|{}|{}", s.endpoint, s.logs_endpoint.as_deref().unwrap_or(""), s.protocol);
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    crate::map::hex(&digest.as_ref()[..8])
}

/// Splits a request's records in half, or `None` when it holds one record or fewer.
fn halves(request: &Request) -> Option<(Request, Request)> {
    match request {
        Request::Logs(r) => {
            let mut a = r.clone();
            let records = &mut a.resource_logs.first_mut()?.scope_logs.first_mut()?.log_records;
            if records.len() < 2 {
                return None;
            }
            let tail = records.split_off(records.len() / 2);
            let mut b: ExportLogsServiceRequest = a.clone();
            b.resource_logs[0].scope_logs[0].log_records = tail;
            Some((Request::Logs(a), Request::Logs(b)))
        }
        Request::Metrics(r) => {
            let mut a = r.clone();
            let metrics = &mut a.resource_metrics.first_mut()?.scope_metrics.first_mut()?.metrics;
            if metrics.len() < 2 {
                return None;
            }
            let tail = metrics.split_off(metrics.len() / 2);
            let mut b: ExportMetricsServiceRequest = a.clone();
            b.resource_metrics[0].scope_metrics[0].metrics = tail;
            Some((Request::Metrics(a), Request::Metrics(b)))
        }
        Request::Traces(_) => None,
    }
}

/// A jittered next delay: the current one doubled, capped, ±20 %.
fn next_backoff(current: Option<Duration>) -> Duration {
    let base = match current {
        None => BACKOFF_START,
        Some(d) => (d * 2).min(BACKOFF_CAP),
    };
    let mut r = [0u8; 1];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut r);
    let factor = 0.8 + 0.4 * (f64::from(r[0]) / 255.0);
    base.mul_f64(factor)
}

impl Exporter {
    pub fn new(contract_path: &Path, headers: Vec<(String, String)>) -> Result<Exporter, String> {
        let contract = contract::load(contract_path)?;
        let own = env!("CARGO_PKG_VERSION");
        if !contract.mothership_version.is_empty() && contract.mothership_version != own {
            return Err(format!(
                "refused: the mothership is {}, this add-on is {own}; install the matching add-on",
                contract.mothership_version
            ));
        }
        let transport = Transport::new(&contract.settings, headers)?;
        let policy = policy_for(&contract);
        let resource = resource_for(&policy, &contract);
        let data_dir = contract.data_dir.clone();
        let (state, gap) = State::load(&data_dir);
        let mut aggregates: Aggregates = state
            .extra
            .get(METRICS_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        if aggregates.start_unix_nanos == 0 {
            aggregates.start_unix_nanos = now_nanos();
        }
        if gap.is_some() {
            aggregates.drop_count("state_reset", 1);
        }
        let endpoint = colonizer_redact::redact_text(&contract.settings.endpoint).into_owned();
        let mut exporter = Exporter {
            destination: destination_hash(&contract),
            contract_mtime: std::fs::metadata(contract_path).and_then(|m| m.modified()).ok(),
            contract_path: contract_path.to_path_buf(),
            contract,
            data_dir,
            policy,
            resource,
            transport,
            state,
            aggregates,
            outbox: None,
            backoff: None,
            last_metrics: None,
            tailer: Tailer::default(),
            last_gaps: Vec::new(),
            status: Status {
                version: env!("CARGO_PKG_VERSION"),
                contract: contract::CONTRACT,
                state: "starting",
                endpoint,
                ..Status::default()
            },
            last_status_write: None,
        };
        exporter.init_signals();
        exporter.settle_start();
        Ok(exporter)
    }

    /// The first time a destination is seen, settles where it starts: with `start_from = now`
    /// every existing file is bound to its end, so nothing historical is sent. Recorded in the
    /// state, so a restart (or a later file) is never skipped again.
    fn settle_start(&mut self) {
        let mut settled: Vec<String> = self
            .state
            .extra
            .get(DESTINATIONS_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        // A destination with read positions from before this setting existed is settled already.
        let known =
            settled.contains(&self.destination) || self.state.cursors.keys().any(|k| k.destination_hash == self.destination);
        if known {
            return;
        }
        if self.contract.settings.start_from != contract::START_BACKLOG {
            let inputs = Inputs {
                data_dir: &self.data_dir,
                settings: &self.contract.settings,
                colonies: &self.contract.policy,
                state: &self.state,
                destination: &self.destination,
                now_unix_nanos: now_nanos(),
            };
            let mut seeded = self.state.clone();
            crate::tailer::seed_at_end(&inputs, &mut seeded);
            self.state = seeded;
        }
        settled.push(self.destination.clone());
        self.state
            .extra
            .insert(DESTINATIONS_KEY.to_string(), serde_json::json!(settled));
        let _ = self.state.commit(&self.data_dir, true);
    }

    /// One health entry per signal, `off` when its streams are switched off.
    fn init_signals(&mut self) {
        let s = &self.contract.settings;
        let logs_on = s.stream_operational || s.stream_activity;
        for (name, path, on) in [("logs", "/v1/logs", logs_on), ("metrics", "/v1/metrics", s.stream_metrics)] {
            let endpoint = colonizer_redact::redact_text(&s.url(path)).into_owned();
            let entry = self.status.signal(name);
            entry.endpoint = endpoint;
            if !on {
                entry.state = "off";
            }
        }
    }

    /// The contract's colony list changes as colonies come and go; the settings never change under
    /// a running exporter (the mothership restarts it for that).
    fn reload_policy(&mut self) {
        let mtime = std::fs::metadata(&self.contract_path).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime != self.contract_mtime {
            if let Ok(fresh) = contract::load(&self.contract_path) {
                self.contract.policy = fresh.policy;
            }
            self.contract_mtime = mtime;
        }
    }

    /// One pass: retry the outbox if one is waiting, else read, map, send and commit; then push
    /// metrics when they are due. Returns how long to wait before the next pass.
    pub async fn tick(&mut self) -> Duration {
        if let Some((at, delay)) = self.backoff
            && at.elapsed() < delay
        {
            return delay - at.elapsed();
        }
        self.reload_policy();
        let before = self.status.state;
        if self.outbox.is_none() {
            self.outbox = Some(self.read());
        }
        let wait = match self.flush().await {
            None => {
                self.backoff = None;
                self.push_metrics().await;
                TICK
            }
            Some(held) => {
                let delay = match held {
                    Held::Retry(Some(after)) => after.max(BACKOFF_START),
                    Held::Retry(None) | Held::Auth => next_backoff(self.backoff.map(|(_, d)| d)),
                };
                self.backoff = Some((Instant::now(), delay));
                self.status.next_retry_unix = Some(unix_now() + delay.as_secs_f64().ceil() as u64);
                delay
            }
        };
        // A change of state (say, to `auth_failed`) is written at once, not after the rate limit.
        self.write_status(self.status.state != before);
        wait
    }

    /// Reads what is new in every source, within this tick's budget, into an outbox.
    fn read(&mut self) -> Outbox {
        let now = now_nanos();
        let inputs = Inputs {
            data_dir: &self.data_dir,
            settings: &self.contract.settings,
            colonies: &self.contract.policy,
            state: &self.state,
            destination: &self.destination,
            now_unix_nanos: now,
        };
        let tailed = self.tailer.collect(&inputs);
        let mut aggregates = self.aggregates.clone();
        for (reason, n) in &tailed.drops {
            aggregates.drop_count(reason, *n);
        }
        let mapper = Mapper {
            policy: &self.policy,
            host_id: &self.contract.host_id,
            colonies: &self.contract.policy,
            now_unix_nanos: now,
        };
        let mut items = Vec::new();
        // Gap records are their own `meta` stream: sent only while a log stream is on.
        let gaps = if sources::logs_enabled(&self.contract.settings, crate::policy::Source::ExportGap) {
            &tailed.gaps[..]
        } else {
            &[]
        };
        for record in gaps.iter().chain(&tailed.records) {
            match record.signal {
                Signal::Metrics => aggregates.fold(record.source, &record.line),
                _ => {
                    if let Some(item) = mapper.log(record.source, record.colony.as_deref(), &record.line, &record.digest) {
                        items.push(item);
                    }
                }
            }
        }
        self.last_gaps = tailed.gaps.iter().map(|g| g.line.clone()).collect();
        let records = items.len() as u64;
        let mut batcher = Batcher::new(
            &self.resource,
            BatchConfig {
                encoding: self.transport.encoding(),
                ..BatchConfig::default()
            },
        );
        for item in items {
            batcher.push(item);
        }
        let batch = batcher.finish();
        aggregates.drop_count("oversized", batch.oversized as u64);
        Outbox {
            requests: batch.requests,
            records,
            cursors: tailed.cursors,
            removed: tailed.removed,
            gone: tailed.gone,
            aggregates,
        }
    }

    /// Sends the outbox; on success commits its cursors and series and returns `None`. On any
    /// failure but a record-level refusal, keeps what is left (cursors uncommitted) and says why.
    async fn flush(&mut self) -> Option<Held> {
        let mut outbox = self.outbox.take()?;
        while let Some(request) = outbox.requests.first().cloned() {
            match self.transport.send(&request).await {
                Outcome::Sent {
                    rejected,
                    message,
                    bytes,
                } => {
                    outbox.aggregates.drop_count("rejected", rejected);
                    outbox.requests.remove(0);
                    let now = unix_now();
                    if rejected > 0 || message.is_some() {
                        self.status.last_partial_success = Some(PartialSuccess {
                            at_unix: now,
                            signal: "logs",
                            rejected,
                            message,
                        });
                    }
                    let accepted = (request.len() as u64).saturating_sub(rejected);
                    self.status.success("logs", now, accepted, bytes);
                }
                Outcome::Refused { message, .. } => {
                    outbox.requests.remove(0);
                    match halves(&request) {
                        Some((a, b)) => {
                            outbox.requests.insert(0, b);
                            outbox.requests.insert(0, a);
                        }
                        None => {
                            outbox.aggregates.drop_count("refused", request.len() as u64);
                            outbox.records = outbox.records.saturating_sub(request.len() as u64);
                        }
                    }
                    self.status.last_error = Some(message);
                }
                Outcome::Unauthorized { message, .. } => {
                    self.aggregates.export_failures += 1;
                    outbox.aggregates.export_failures = self.aggregates.export_failures;
                    self.status.failure("logs", "auth_failed", message);
                    self.status.state = "auth_failed";
                    self.outbox = Some(outbox);
                    return Some(Held::Auth);
                }
                Outcome::Retry { message, after } => {
                    self.aggregates.export_failures += 1;
                    outbox.aggregates.export_failures = self.aggregates.export_failures;
                    self.status.failure("logs", "backing_off", message);
                    self.status.state = "retrying";
                    self.outbox = Some(outbox);
                    return Some(Held::Retry(after));
                }
            }
        }
        // Everything acknowledged: commit the cursors and the series in one write.
        let changed = !outbox.cursors.is_empty()
            || !outbox.removed.is_empty()
            || !outbox.gone.is_empty()
            || outbox.aggregates != self.aggregates;
        outbox.aggregates.exported += outbox.records;
        for (key, cursor) in outbox.cursors {
            self.state.set_cursor(key, cursor);
        }
        for key in &outbox.removed {
            self.state.cursors.remove(key);
        }
        if !outbox.gone.is_empty() {
            // Only colonies the mothership still lists need remembering; one it dropped has no
            // read positions left, so nothing would look for it again.
            let mut gone = crate::tailer::gone(&self.state);
            gone.extend(outbox.gone);
            gone.retain(|id| self.contract.policy.contains_key(id));
            self.state
                .extra
                .insert(crate::tailer::GONE_KEY.to_string(), serde_json::json!(gone));
        }
        self.aggregates = outbox.aggregates;
        self.state.extra.insert(
            METRICS_KEY.to_string(),
            serde_json::to_value(&self.aggregates).unwrap_or_default(),
        );
        if changed {
            // Forced: the cursors of an acknowledged batch are written now, in one atomic write
            // with the series, so a crash after an ack replays at most the batch in flight.
            let _ = self.state.commit(&self.data_dir, true);
        }
        self.status.state = "running";
        None
    }

    /// Pushes every metric point when the interval has passed. Points are cumulative, so a failed
    /// push is simply superseded by the next one; it is counted, never retried.
    async fn push_metrics(&mut self) {
        let s = &self.contract.settings;
        if !s.stream_metrics {
            return;
        }
        let interval = Duration::from_secs(s.metrics_interval_secs.max(5));
        if self.last_metrics.is_some_and(|t| t.elapsed() < interval) {
            return;
        }
        self.last_metrics = Some(Instant::now());
        let points = self.aggregates.points(&self.policy, &self.contract.policy, now_nanos());
        let mut batcher = Batcher::new(
            &self.resource,
            BatchConfig {
                encoding: self.transport.encoding(),
                ..BatchConfig::default()
            },
        );
        for p in points {
            batcher.push(p);
        }
        for request in batcher.finish().requests {
            let records = request.len() as u64;
            match self.transport.send(&request).await {
                Outcome::Sent {
                    rejected,
                    message,
                    bytes,
                } => {
                    let now = unix_now();
                    if rejected > 0 || message.is_some() {
                        self.status.last_partial_success = Some(PartialSuccess {
                            at_unix: now,
                            signal: "metrics",
                            rejected,
                            message,
                        });
                    }
                    self.status.success("metrics", now, records.saturating_sub(rejected), bytes);
                }
                Outcome::Unauthorized { message, .. } => {
                    self.aggregates.export_failures += 1;
                    self.status.failure("metrics", "auth_failed", message);
                    break;
                }
                Outcome::Retry { message, .. } | Outcome::Refused { message, .. } => {
                    self.aggregates.export_failures += 1;
                    self.status.failure("metrics", "backing_off", message);
                    break;
                }
            }
        }
    }

    /// Writes `status.json`, at most once a second unless `force`.
    pub fn write_status(&mut self, force: bool) {
        if !force && self.last_status_write.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.status.heartbeat_unix = unix_now();
        self.status.exported = self.aggregates.exported;
        self.status.backlog_bytes = self.backlog();
        self.status.dropped = self.aggregates.dropped.clone();
        self.status.export_failures = self.aggregates.export_failures;
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.status) {
            let path = status_path(&self.data_dir);
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = crate::write_atomic(&path, &bytes);
        }
        self.last_status_write = Some(Instant::now());
    }

    /// The ledger bytes not yet behind a committed cursor, over every source.
    fn backlog(&self) -> u64 {
        let gone = crate::tailer::gone(&self.state);
        let colonies: Vec<String> = self
            .contract
            .policy
            .keys()
            .filter(|id| !gone.contains(*id))
            .cloned()
            .collect();
        sources::discover(&self.data_dir, &self.contract.settings, &colonies)
            .iter()
            .map(|file| backlog_bytes(file, &self.state.cursor(&crate::tailer::key(&self.destination, file))))
            .sum()
    }

    /// The last commit, forced, and a final status: called on a clean shutdown.
    pub fn shutdown(&mut self) {
        let _ = self.state.commit(&self.data_dir, true);
        self.status.state = "stopped";
        self.write_status(true);
    }
}

/// `<data>/observability/status.json`.
pub fn status_path(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join("status.json")
}

/// `send-test-event`: one log record and one metric point to the configured backend, and the
/// outcome of each, as JSON for the mothership's "Send test" button.
pub async fn send_test(contract: &Contract, headers: Vec<(String, String)>) -> serde_json::Value {
    let transport = match Transport::new(&contract.settings, headers) {
        Ok(t) => t,
        Err(e) => return serde_json::json!({"ok": false, "error": e}),
    };
    let policy = policy_for(contract);
    let resource = resource_for(&policy, contract);
    let now = now_nanos();
    let id = crate::map::record_id(
        &contract.host_id,
        crate::policy::Source::Mothership,
        "-",
        &format!("test-{now}"),
    );
    let log = policy
        .log(crate::policy::Source::Mothership)
        .time(now)
        .severity(crate::proto::logs::v1::SeverityNumber::Info)
        .event_name("colonizer.test")
        .attr("colonizer.record.id", id, crate::policy::Tier::Structure)
        .attr("colonizer.source", "mothership", crate::policy::Tier::Structure)
        .attr("colonizer.stream", "operational", crate::policy::Tier::Structure)
        .body("Colonizer observability test event", crate::policy::Tier::Structure)
        .finish();
    let metric = policy
        .metric(crate::policy::Source::Mothership, "colonizer.observability.test", "{event}")
        .times(now, now)
        .sum(true)
        .int(1)
        .finish();
    let mut results = serde_json::Map::new();
    let mut ok = true;
    for (name, item) in [("logs", log), ("metrics", metric)] {
        let mut batcher = Batcher::new(
            &resource,
            BatchConfig {
                encoding: transport.encoding(),
                ..BatchConfig::default()
            },
        );
        batcher.push(item);
        let Some(request) = batcher.finish().requests.into_iter().next() else {
            continue;
        };
        let result = match transport.send(&request).await {
            Outcome::Sent { rejected, message, .. } => {
                serde_json::json!({"ok": rejected == 0, "rejected": rejected, "message": message})
            }
            Outcome::Retry { message, .. } => serde_json::json!({"ok": false, "error": message}),
            Outcome::Unauthorized { status, message } => {
                serde_json::json!({"ok": false, "status": status, "auth_failed": true, "error": message})
            }
            Outcome::Refused { status, message } => serde_json::json!({"ok": false, "status": status, "error": message}),
        };
        ok &= result["ok"] == true;
        results.insert(name.to_string(), result);
    }
    serde_json::json!({"ok": ok, "signals": results})
}

#[cfg(test)]
mod tests;
