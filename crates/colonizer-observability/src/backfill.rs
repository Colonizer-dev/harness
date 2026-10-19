//! The backfill engine (#858): a one-shot replay of the ledgers' on-disk history to the configured
//! OTLP backend. The mothership invokes it and reads one JSON summary from stdout
//! (docs/design/observability.md, contract part (e)); it never runs beside its own state.
//!
//! - **Bounded by the live exporter.** Every selected file is replayed from its start up to a
//!   per-file bound: the live exporter's committed position once the destination is settled, else
//!   the file's last complete line at go-live. The two readers never overlap, and the backfill
//!   never writes the live `state.json` — its cursors and counters live in
//!   `observability/backfill.json`, written in the same atomic write as the cursors it commits.
//! - **The same pipeline** as the live exporter: policy, mapper, trace builder, batcher,
//!   transport. Record ids are deterministic, so a backend that dedupes absorbs any replay.
//! - **The archive half** (`--include-archive`, #859) replays the tar+zstd bundles of colonies the
//!   disk no longer holds, through the same window, mapper and trace builder under the bundles'
//!   own host ids. It keeps no progress file: a resumed run replays the bundles from the top, and
//!   the ids being deterministic, the backend absorbs that like any replay.
//! - **Metrics are skipped on purpose**: the live exporter's metric series are cumulative folds of
//!   their ledgers, and replaying the same inputs would double-count every point. A backfill of
//!   logs and traces is the whole feature.
//! - **Old records skip the backlog guard**: `max_backlog_days` never applies here. `--since` and
//!   `--until` bound the window instead, and the send rate is held to `--rate` records a second
//!   (`--dry-run` counts and estimates without sending anything).

use crate::archive_source::{self, ArchivedColony, Message};
use crate::batch::{BatchConfig, Batcher, ExportResource, Item};
use crate::contract::{self, ColonyPolicy, Contract};
use crate::cursor::{Cursor, GapReason, Limits, read_batch};
use crate::encode::Request;
use crate::exporter::{self, now_nanos, policy_for, resource_for};
use crate::map::{Mapper, ts_nanos};
use crate::policy::{Policy, Source};
use crate::sources::{self, SourceFile};
use crate::state::{CursorKey, Signal, State};
use crate::tailer;
use crate::traces::{Builder, Traces};
use crate::transport::{Outcome, Transport};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver as ArchiveReceiver, TryRecvError};

use std::time::Duration;

pub(crate) mod flags;
mod pace;
mod progress;

// The pieces the one-shot is split into, so the engine itself stays one readable file: the CLI
// grammar, the send pacing, the progress file. Their items live under their engine paths.
use flags::DEFAULT_RATE;
pub use flags::Options;
use pace::{BACKOFF_START, Bucket, next_backoff};
pub(crate) use progress::Window;
pub use progress::progress_path;
use progress::{CursorEntry, Progress};

/// The pause between ticks: a full rate bucket refills in one second.
const TICK: Duration = Duration::from_secs(1);
/// The bucket colonies with no org of their own are grouped under in `per_org`.
const INSTALL_ORG: &str = "(install)";
/// The longest line read as a line: the live tailer's own cap, and the archive reader's too.
pub(crate) const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// The smallest byte share one file gets in a pass, so a pass over many files still reads lines.
const MIN_SHARE: u64 = 16 * 1024;
/// The key the live exporter records its settled destinations under in `state.json`'s `extra`. The
/// exporter's own constant is private to it; the literal is shared, not imported.
const DESTINATIONS_KEY: &str = "destinations";

/// One file to replay, and the offset its replay stops at: the bytes past it belong to the live
/// stream.
struct Plan {
    file: SourceFile,
    until: u64,
}

/// How the run ended, or that it has not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunState {
    Running,
    Complete,
    Cancelled,
    Failed,
}

impl RunState {
    fn as_str(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Complete => "complete",
            RunState::Cancelled => "cancelled",
            RunState::Failed => "failed",
        }
    }
}

/// A per-signal tally of the replay.
#[derive(Default)]
struct SignalCount {
    records: u64,
    ledger_bytes: u64,
    encoded_bytes: u64,
}

/// A per-org tally of the replay.
#[derive(Default)]
struct OrgCount {
    records: u64,
    ledger_bytes: u64,
}

/// Everything the summary reports, in memory. The per-signal and per-org tallies count what was
/// read and built (a dry run's numbers by construction); `records_sent` and `bytes_sent` count
/// what the backend acknowledged.
#[derive(Default)]
struct Counters {
    records_sent: u64,
    bytes_sent: u64,
    malformed: u64,
    untimestamped: u64,
    out_of_window: u64,
    refused: u64,
    per_signal: BTreeMap<&'static str, SignalCount>,
    per_org: BTreeMap<String, OrgCount>,
    /// Every line kept, in bytes: the ETA's denominator with `records_built`.
    ledger_read: u64,
    records_built: u64,
}

impl Counters {
    /// One kept line of `signal`'s files, `size` bytes on disk, read for `org`'s colonies.
    fn kept(&mut self, signal: &'static str, org: &str, size: u64) {
        self.per_signal.entry(signal).or_default().ledger_bytes += size;
        self.per_org.entry(org.to_string()).or_default().records += 1;
        self.per_org.get_mut(org).unwrap().ledger_bytes += size;
        self.ledger_read += size;
    }
}

/// The org a per-colony file's lines are tallied under: the colony's org, or `"(install)"` for the
/// install-wide files and a colony with no org of its own.
fn org_of(colonies: &BTreeMap<String, ColonyPolicy>, colony: Option<&str>) -> String {
    colony
        .and_then(|c| colonies.get(c))
        .map(|p| p.org.as_str())
        .filter(|org| !org.is_empty())
        .unwrap_or(INSTALL_ORG)
        .to_string()
}

/// Requests built but not yet acknowledged, and what to commit once they are.
struct Outbox {
    requests: Vec<Request>,
    cursors: Vec<(CursorKey, Cursor)>,
    traces: Traces,
    /// The trace state of the fleet hosts the archive supplied, keyed by host id. Never persisted:
    /// the archive half replays from the top, so these live and die with the run.
    fleet: BTreeMap<String, Traces>,
}

/// How a delivery pass ended: `Ok` when every request was acknowledged, else why the rest is held.
type Delivery = Result<Outbox, Held>;

/// A delivery that did not finish.
enum Held {
    /// Retry after a backoff, or the server's `Retry-After` when it gave one.
    Retry(Option<Duration>),
    /// The credential was refused: nothing is committed and the run fails.
    Failed(String),
}

/// The engine. Build one with [`Backfill::new`], drive [`Backfill::tick`] until
/// [`Backfill::done`], then read [`Backfill::summary`]; [`run`] does all three.
pub struct Backfill {
    contract: Contract,
    data_dir: PathBuf,
    policy: Policy,
    resource: ExportResource,
    transport: Transport,
    destination: String,
    rate: u64,
    dry_run: bool,
    include_archive: bool,
    window: Window,
    repo: Option<String>,
    /// Sorted by `relative` for a deterministic rotation; metrics never appear.
    files: Vec<Plan>,
    /// The colonies the archive supplies under `--include-archive`, in id order.
    archive: Vec<ArchivedColony>,
    /// The archive reader's chunks, when there are bundles to read.
    archive_rx: Option<ArchiveReceiver<Message>>,
    /// A chunk this tick's budget could not finish; the next tick's starts here.
    archive_pending: Option<Message>,
    /// The reader has signed off (`Done` seen, or the channel gone quiet without it).
    archive_done: bool,
    /// The trace state of every fleet host but this one, keyed by host id. Never persisted.
    fleet_traces: BTreeMap<String, Traces>,
    /// The backfill's own cursors, committed only after the backend's ack.
    cursors: BTreeMap<CursorKey, Cursor>,
    traces: Traces,
    outbox: Option<Outbox>,
    backoff: Option<(tokio::time::Instant, Duration)>,
    records_bucket: Bucket,
    bytes_bucket: Bucket,
    rotation: usize,
    state: RunState,
    error: Option<String>,
    counters: Counters,
    started_unix: u64,
    last_write: Option<std::time::Instant>,
    cancelled: bool,
}

impl Backfill {
    /// Builds the plan: the contract's colonies (narrowed by `--repo`), every logs and traces
    /// source of theirs, each bounded where the live exporter's half of the file begins. A
    /// progress file of the very same plan is resumed; a completed one simply reports done.
    pub fn new(contract_path: &Path, headers: Vec<(String, String)>, opts: &Options) -> Result<Backfill, String> {
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
        let destination = exporter::destination_hash(&contract);

        let colonies: Vec<String> = contract
            .policy
            .iter()
            .filter(|(_, p)| opts.repo.as_deref().is_none_or(|r| p.repo.eq_ignore_ascii_case(r)))
            .map(|(id, _)| id.clone())
            .collect();
        // The live state decides where history ends and the live stream begins. It is read, never
        // written: the backfill keeps its own cursors in backfill.json.
        let (live, _) = State::load(&data_dir);
        let settled = live.cursors.keys().any(|k| k.destination_hash == destination)
            || live
                .extra
                .get(DESTINATIONS_KEY)
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .is_some_and(|list| list.contains(&destination));
        let bound = |file: &SourceFile| -> Option<u64> {
            if settled {
                // The live exporter owns everything past its committed position. A file it has no
                // position for appeared after the destination went live: it is all the live
                // stream's, and the replay skips it.
                let cursor = live.cursor(&tailer::key(&destination, file));
                cursor.file_id.is_some().then_some(cursor.offset)
            } else {
                // The destination is not live yet: the exporter is about to seed at each file's
                // last complete line, which makes this replay's go-live point.
                crate::cursor::at_end(&file.live).ok().flatten().map(|c| c.offset)
            }
        };
        let mut files: Vec<Plan> = sources::discover(&data_dir, &contract.settings, &colonies)
            .into_iter()
            // Metrics never appear: their series are cumulative folds, and a replay of the inputs
            // would double-count every point.
            .filter(|file| matches!(file.signal, Signal::Logs | Signal::Traces))
            .filter_map(|file| {
                Some(Plan {
                    until: bound(&file).filter(|until| *until > 0)?,
                    file,
                })
            })
            .collect();
        files.sort_by(|a, b| a.file.relative.cmp(&b.file.relative));

        // The archive half (#859): the colonies the disk no longer holds, discovered from the
        // bundles' sidecars. Disk wins — a colony with a live session directory is never read out
        // of a bundle — and an archived colony exports structure only: its synthesized policy
        // fails the content gate whatever the contract says (P3).
        let mut archive = Vec::new();
        let mut archive_rx = None;
        if opts.include_archive {
            let on_disk: BTreeSet<String> = colonies
                .iter()
                .filter(|id| data_dir.join("sessions").join(id).is_dir())
                .cloned()
                .collect();
            archive = archive_source::discover(&data_dir, opts.repo.as_deref(), &on_disk)
                .values()
                .cloned()
                .collect();
            if !archive.is_empty() {
                // The reader owns the whole decoder on its own thread and blocks on a full
                // channel, so it never runs ahead of this run's rate budget. There is no progress
                // file for it: a resumed run replays the bundles from the top, and the ids being
                // deterministic, the backend absorbs that like any replay.
                let (tx, rx) = archive_source::channel();
                let bundles = archive.clone();
                std::thread::Builder::new()
                    .name("observability-archive".into())
                    .spawn(move || archive_source::stream(&bundles, tx))
                    .map_err(|e| format!("cannot start the archive reader: {e}"))?;
                archive_rx = Some(rx);
            }
        }

        let window = Window {
            since: opts.since,
            until: opts.until,
        };
        let until_offsets: BTreeMap<String, u64> = files.iter().map(|p| (p.file.relative.clone(), p.until)).collect();
        // Whether the archive half is over before it began: no bundles, or none asked for.
        let archive_done = archive_rx.is_none();
        let mut backfill = Backfill {
            contract,
            data_dir,
            policy,
            resource,
            transport,
            destination,
            rate: if opts.rate == 0 { DEFAULT_RATE } else { opts.rate },
            dry_run: opts.dry_run,
            include_archive: opts.include_archive,
            window,
            repo: opts.repo.clone(),
            files,
            archive,
            archive_rx,
            archive_pending: None,
            archive_done,
            fleet_traces: BTreeMap::new(),
            cursors: BTreeMap::new(),
            traces: Traces::default(),
            outbox: None,
            backoff: None,
            records_bucket: Bucket::default(),
            bytes_bucket: Bucket::default(),
            rotation: 0,
            state: RunState::Running,
            error: None,
            counters: Counters::default(),
            started_unix: now_nanos() / 1_000_000_000,
            last_write: None,
            cancelled: false,
        };
        // A progress file of the very same plan (destination, window, bounds) is the only one
        // resumed; anything else — absent, corrupt, or a different plan — starts from the top.
        let resumed = std::fs::read(progress_path(&backfill.data_dir))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Progress>(&bytes).ok())
            .filter(|p| p.destination == backfill.destination && p.window == window && p.until_offsets == until_offsets);
        if let Some(p) = resumed {
            for entry in p.cursors {
                backfill.cursors.insert(entry.key(), entry.cursor);
            }
            // The bounds only ever match a plan computed over the same bytes, but a live cursor
            // may have moved within them: a cursor past its bound is a file already done.
            for plan in &backfill.files {
                let key = tailer::key(&backfill.destination, &plan.file);
                if let Some(cursor) = backfill.cursors.get_mut(&key) {
                    cursor.offset = cursor.offset.min(plan.until);
                }
            }
            backfill.traces = p.traces.get(&backfill.destination).cloned().unwrap_or_default();
            backfill.counters.records_sent = p.records_sent;
            backfill.counters.bytes_sent = p.bytes_sent;
            backfill.started_unix = p.started_unix;
            // A crashed ("running") or cancelled run resumes from its cursors; a failed one
            // retries with a clean slate of failures. A completed one is simply done again: a
            // second invocation of the same plan sends nothing.
            if p.state == "complete" {
                backfill.state = RunState::Complete;
            }
        }
        Ok(backfill)
    }

    /// One pass: retry the outbox if one is waiting, else read what the rate allows, map it and
    /// send it. Returns how long to wait before the next pass.
    pub async fn tick(&mut self) -> Duration {
        if self.state != RunState::Running {
            return Duration::ZERO;
        }
        if let Some((at, delay)) = self.backoff
            && at.elapsed() < delay
        {
            return delay - at.elapsed();
        }
        // Cancelled: what was acknowledged stays committed, the in-flight batch is dropped.
        if self.cancelled {
            self.finish(RunState::Cancelled);
            return Duration::ZERO;
        }
        if self.outbox.is_none() {
            let outbox = self.read();
            if self.dry_run {
                // Nothing is sent or written: the cursors live in memory alone, and the run
                // ends the moment every one of them reaches its bound.
                self.cursors.extend(outbox.cursors);
                self.traces = outbox.traces;
                self.fleet_traces = outbox.fleet;
                if self.all_done() {
                    self.finish(RunState::Complete);
                    return Duration::ZERO;
                }
                return TICK;
            }
            self.outbox = Some(outbox);
        }
        let outbox = self.outbox.take().expect("just set");
        match self.deliver(outbox).await {
            Delivery::Ok(mut outbox) => {
                self.backoff = None;
                for (key, cursor) in outbox.cursors.drain(..) {
                    self.cursors.insert(key, cursor);
                }
                // The open spans land in the same write as the cursors that read them, so a
                // crash replays at most the batch in flight.
                self.traces = outbox.traces;
                self.fleet_traces = outbox.fleet;
                self.write_progress(true);
                if self.all_done() {
                    let end = if self.cancelled {
                        RunState::Cancelled
                    } else {
                        RunState::Complete
                    };
                    self.finish(end);
                    return Duration::ZERO;
                }
                TICK
            }
            Delivery::Err(Held::Retry(after)) => {
                let delay = match after {
                    Some(after) => after.max(BACKOFF_START),
                    None => next_backoff(self.backoff.map(|(_, d)| d)),
                };
                self.backoff = Some((tokio::time::Instant::now(), delay));
                delay
            }
            Delivery::Err(Held::Failed(message)) => {
                // Nothing is committed: the next run resumes from the last acknowledged
                // cursors, once the key works. The unacknowledged batch is dropped with the run.
                self.error = Some(message);
                self.finish(RunState::Failed);
                Duration::ZERO
            }
        }
    }

    /// Reads what this tick's budget allows from every file with history left, maps it through
    /// the live exporter's own pipeline, and packs it into requests.
    fn read(&mut self) -> Outbox {
        let now = now_nanos();
        // Taken out of `self` so the mapper and the builder can share the policy and the colony
        // list while the loop mutates the tallies.
        let mut counters = std::mem::take(&mut self.counters);
        let mut records_bucket = std::mem::take(&mut self.records_bucket);
        let mut bytes_bucket = std::mem::take(&mut self.bytes_bucket);
        let byte_rate = self.contract.settings.max_read_mib_per_sec.max(1).saturating_mul(1024 * 1024);
        let mut records = records_bucket.available(self.rate);
        let mut bytes = bytes_bucket.available(byte_rate);

        // The files with history left, in this tick's rotation.
        let n = self.files.len();
        let start = if n == 0 { 0 } else { self.rotation % n };
        self.rotation = self.rotation.wrapping_add(1);
        let mut working: BTreeMap<usize, Cursor> = BTreeMap::new();
        let mut active: Vec<usize> = Vec::new();
        for i in 0..n {
            let at = (start + i) % n;
            let cursor = self
                .cursors
                .get(&tailer::key(&self.destination, &self.files[at].file))
                .cloned()
                .unwrap_or_default();
            if cursor.offset >= self.files[at].until {
                continue;
            }
            working.insert(at, cursor);
            active.push(at);
        }

        // The colonies the archive supplies ride along under their synthesized, fail-closed
        // policies, so the mapper, the builders and the org tallies see one colony list; the ones
        // another host archived are keyed by colony id for the redirect below.
        let mut colonies_map = self.contract.policy.clone();
        for archived in &self.archive {
            colonies_map.insert(archived.colony.clone(), archived.policy.clone());
        }
        let archive_hosts: BTreeMap<&str, &str> = self.archive.iter().map(|a| (a.colony.as_str(), a.host.as_str())).collect();

        let mapper = Mapper {
            policy: &self.policy,
            host_id: &self.contract.host_id,
            colonies: &colonies_map,
            now_unix_nanos: now,
        };
        let builder = Builder {
            policy: &self.policy,
            host_id: &self.contract.host_id,
            colonies: &colonies_map,
            sample_ratio: self.contract.settings.trace_sample_ratio,
            max_trace_bytes: self.contract.settings.max_trace_bytes,
        };
        let mut traces = self.traces.clone();
        let mut fleet = self.fleet_traces.clone();
        let mut items: Vec<Item> = Vec::new();
        let mut cursors = Vec::new();

        while records > 0 && bytes > 0 && !active.is_empty() {
            let share_bytes = (bytes / active.len() as u64).max(MIN_SHARE);
            let share_lines = ((records as usize) / active.len()).max(1);
            let mut again = Vec::new();
            for at in active {
                if records == 0 || bytes == 0 {
                    break;
                }
                let plan = &self.files[at];
                let key = tailer::key(&self.destination, &plan.file);
                let cursor = working[&at].clone();
                // The bound caps the batch's byte budget too, so a read can never cross into
                // the live stream's half of the file.
                let headroom = plan.until.saturating_sub(cursor.offset);
                let limits = Limits {
                    max_bytes: share_bytes.min(bytes).min(headroom),
                    max_lines: share_lines.min(records as usize),
                    max_line_bytes: MAX_LINE_BYTES,
                };
                let Ok(batch) = read_batch(&plan.file.live, plan.file.rolled.as_deref(), &cursor, &limits) else {
                    continue;
                };
                bytes = bytes.saturating_sub(batch.bytes_read);
                records = records.saturating_sub(batch.lines.len() as u64);
                records_bucket.spend(batch.lines.len() as u64);
                bytes_bucket.spend(batch.bytes_read);
                counters.malformed += batch.malformed;
                // A cursor gap (a rotation the reader fell behind, a truncation) means the
                // bounded region can no longer be read linearly: the live stream owns the
                // discrepancy and this file's replay is done, not restarted.
                let gapped = batch.gaps.iter().any(|g| !matches!(g.reason, GapReason::StateReset));
                let org = org_of(&colonies_map, plan.file.colony.as_deref());

                let mut line_end = cursor.offset;
                for ((line, digest), size) in batch.lines.iter().zip(&batch.digests).zip(&batch.sizes) {
                    line_end += size;
                    if line_end > plan.until {
                        // A line crossing the bound (a partial last line at go-live, since
                        // completed) belongs to the live stream, which reads it whole.
                        break;
                    }
                    // The window bounds the replay where the backlog guard bounds the live
                    // stream; a line with no ts cannot be placed in one.
                    if self.window.bounded() {
                        match ts_nanos(line) {
                            Some(ts) if self.window.keeps(ts) => {}
                            None => {
                                counters.untimestamped += 1;
                                continue;
                            }
                            Some(_) => {
                                counters.out_of_window += 1;
                                continue;
                            }
                        }
                    }
                    match plan.file.signal {
                        Signal::Logs => {
                            if let Some(item) = mapper.log(plan.file.source, plan.file.colony.as_deref(), line, digest) {
                                counters.kept("logs", &org, *size);
                                items.push(item);
                            }
                        }
                        Signal::Traces => {
                            // An activity outcome for a colony another host archived closes that
                            // host's root span: its ids are keyed by the bundle's mothership host,
                            // and its trace state is the fleet map's, never the persisted one.
                            let other_host: Option<&str> = if plan.file.source == Source::Activity {
                                line.get("colony")
                                    .and_then(Value::as_str)
                                    .and_then(|c| archive_hosts.get(c).copied())
                                    .filter(|host| *host != self.contract.host_id.as_str())
                            } else {
                                None
                            };
                            match other_host {
                                Some(host) => {
                                    let fleet_builder = Builder {
                                        policy: &self.policy,
                                        host_id: host,
                                        colonies: &colonies_map,
                                        sample_ratio: self.contract.settings.trace_sample_ratio,
                                        max_trace_bytes: self.contract.settings.max_trace_bytes,
                                    };
                                    let state = fleet.entry(host.to_string()).or_default();
                                    fleet_builder.feed(
                                        state,
                                        plan.file.source,
                                        plan.file.colony.as_deref(),
                                        line,
                                        digest,
                                        &mut items,
                                    );
                                }
                                None => {
                                    builder.feed(
                                        &mut traces,
                                        plan.file.source,
                                        plan.file.colony.as_deref(),
                                        line,
                                        digest,
                                        &mut items,
                                    );
                                }
                            }
                            counters.kept("traces", &org, *size);
                        }
                        // Metrics are filtered out of the plan before any of this.
                        Signal::Metrics => {}
                    }
                }

                let mut next = batch.cursor.clone();
                next.offset = next.offset.min(plan.until);
                let done = gapped || next.offset >= plan.until;
                working.insert(at, next.clone());
                if next != cursor {
                    cursors.push((key, next));
                }
                if !done && batch.bytes_read > 0 {
                    again.push(at);
                }
            }
            active = again;
        }

        // The archive half: the chunks the reader thread has ready, within whatever budget the
        // disk files left. An empty channel is not the end — the reader may still be working — so
        // the tick simply comes back for the rest.
        while records > 0 && bytes > 0 {
            let message = match self.archive_pending.take() {
                Some(message) => message,
                None => match self.archive_rx.as_ref().map(|rx| rx.try_recv()) {
                    Some(Ok(message)) => message,
                    Some(Err(TryRecvError::Empty)) => break,
                    // The producer signs off with `Done`; a channel gone quiet without it died,
                    // and a replay must not hang on a dead reader.
                    Some(Err(TryRecvError::Disconnected)) => {
                        self.archive_done = true;
                        break;
                    }
                    None => break,
                },
            };
            match message {
                Message::Lines { colony, host, lines } => {
                    let org = org_of(&colonies_map, Some(&colony));
                    let archive_mapper = Mapper {
                        policy: &self.policy,
                        host_id: &host,
                        colonies: &colonies_map,
                        now_unix_nanos: now,
                    };
                    let archive_builder = Builder {
                        policy: &self.policy,
                        host_id: &host,
                        colonies: &colonies_map,
                        sample_ratio: self.contract.settings.trace_sample_ratio,
                        max_trace_bytes: self.contract.settings.max_trace_bytes,
                    };
                    // A colony this very host archived shares its trace state with the disk half;
                    // another host's is the fleet map's, under that host's ids.
                    let state = if host == self.contract.host_id {
                        &mut traces
                    } else {
                        fleet.entry(host.clone()).or_default()
                    };
                    let settings = &self.contract.settings;
                    let mut held = false;
                    let mut lines = lines.into_iter();
                    for line in lines.by_ref() {
                        if records == 0 || bytes == 0 {
                            // The rest of the chunk waits for the next tick's budget.
                            held = true;
                            break;
                        }
                        records -= 1;
                        bytes = bytes.saturating_sub(line.size);
                        records_bucket.spend(1);
                        bytes_bucket.spend(line.size);
                        if self.window.bounded() {
                            match ts_nanos(&line.line) {
                                Some(ts) if self.window.keeps(ts) => {}
                                None => {
                                    counters.untimestamped += 1;
                                    continue;
                                }
                                Some(_) => {
                                    counters.out_of_window += 1;
                                    continue;
                                }
                            }
                        }
                        if sources::logs_enabled(settings, line.source)
                            && let Some(item) = archive_mapper.log(line.source, Some(&colony), &line.line, &line.digest)
                        {
                            counters.kept("logs", &org, line.size);
                            items.push(item);
                        }
                        if matches!(line.source, Source::Events | Source::Gateway) && settings.stream_traces {
                            archive_builder.feed(state, line.source, Some(&colony), &line.line, &line.digest, &mut items);
                            counters.kept("traces", &org, line.size);
                        }
                    }
                    if held {
                        self.archive_pending = Some(Message::Lines {
                            colony,
                            host,
                            lines: lines.collect(),
                        });
                        break;
                    }
                }
                Message::Done { malformed } => {
                    counters.malformed += malformed;
                    self.archive_done = true;
                }
            }
        }

        // A root goes out once its colony's events are replayed to their bound, so no span of it
        // is closed as incomplete while its real end is still on disk.
        let working_ref = &working;
        let files = &self.files;
        let caught_up = |colony: &str| {
            let relative = format!("sessions/{colony}/events.jsonl");
            files
                .iter()
                .position(|p| p.file.signal == Signal::Traces && p.file.relative == relative)
                .is_none_or(|at| working_ref.get(&at).is_none_or(|c| c.offset >= files[at].until))
        };
        builder.settle(&mut traces, &caught_up, &[], now, &mut items);
        // The fleet hosts' roots go out once their bundles are drained — their caught-up is the
        // stream's own end, since a bundle is only ever read whole.
        if self.archive_done && self.archive_pending.is_none() {
            for (host, state) in fleet.iter_mut() {
                let fleet_builder = Builder {
                    policy: &self.policy,
                    host_id: host,
                    colonies: &colonies_map,
                    sample_ratio: self.contract.settings.trace_sample_ratio,
                    max_trace_bytes: self.contract.settings.max_trace_bytes,
                };
                fleet_builder.settle(state, &|_| true, &[], now, &mut items);
            }
        }

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
        for request in &batch.requests {
            // Only the logs and traces lanes are ever filled; the path says which is which.
            let signal = if request.path() == "/v1/logs" { "logs" } else { "traces" };
            let entry = counters.per_signal.entry(signal).or_default();
            let encoded = request.encoded_len(self.transport.encoding()) as u64;
            entry.records += request.len() as u64;
            entry.encoded_bytes += encoded;
            counters.records_built += request.len() as u64;
            if self.dry_run {
                // What a real run of this very plan would send.
                counters.records_sent += request.len() as u64;
                counters.bytes_sent += encoded;
            }
        }
        self.counters = counters;
        self.records_bucket = records_bucket;
        self.bytes_bucket = bytes_bucket;
        Outbox {
            requests: batch.requests,
            cursors,
            traces,
            fleet,
        }
    }

    /// Sends the outbox's requests in order; on a full acknowledgement returns it for commit. A
    /// record-level refusal is bisected down to the record at fault (the exporter's own halves);
    /// a refused credential fails the run with nothing committed.
    async fn deliver(&mut self, mut outbox: Outbox) -> Delivery {
        while let Some(request) = outbox.requests.first().cloned() {
            match self.transport.send(&request).await {
                Outcome::Sent { rejected, bytes, .. } => {
                    outbox.requests.remove(0);
                    self.counters.records_sent += (request.len() as u64).saturating_sub(rejected);
                    self.counters.bytes_sent += bytes;
                }
                Outcome::Refused { .. } => {
                    outbox.requests.remove(0);
                    match exporter::halves(&request) {
                        Some((a, b)) => {
                            outbox.requests.insert(0, b);
                            outbox.requests.insert(0, a);
                        }
                        None => self.counters.refused += request.len() as u64,
                    }
                }
                Outcome::Unauthorized { message, .. } => return Err(Held::Failed(message)),
                Outcome::Retry { after, .. } => return Err(Held::Retry(after)),
            }
        }
        Ok(outbox)
    }

    /// Whether a file's committed cursor has reached its bound.
    fn reached(&self, plan: &Plan) -> bool {
        self.cursors
            .get(&tailer::key(&self.destination, &plan.file))
            .is_some_and(|c| c.offset >= plan.until)
    }

    fn all_done(&self) -> bool {
        // The archive half is done when its reader has signed off and the last held chunk is
        // consumed; an empty channel may still be filling.
        self.files.iter().all(|plan| self.reached(plan))
            && (self.archive_rx.is_none() || (self.archive_done && self.archive_pending.is_none()))
    }

    fn sources_done(&self) -> usize {
        self.files.iter().filter(|plan| self.reached(plan)).count()
    }

    /// Records the run's end. A dry run leaves no file behind.
    fn finish(&mut self, state: RunState) {
        self.outbox = None;
        self.state = state;
        if !self.dry_run {
            self.write_progress(true);
        }
    }

    /// Writes `backfill.json`, at most once a second unless forced. Every acknowledged batch and
    /// every state change forces one, so a crash replays at most the batch in flight.
    fn write_progress(&mut self, force: bool) {
        let at = std::time::Instant::now();
        if !force && self.last_write.is_some_and(|t| at.duration_since(t) < Duration::from_secs(1)) {
            return;
        }
        let progress = Progress {
            version: 1,
            destination: self.destination.clone(),
            state: self.state.as_str().to_string(),
            error: self.error.clone(),
            window: self.window,
            until_offsets: self.files.iter().map(|p| (p.file.relative.clone(), p.until)).collect(),
            cursors: self.cursors.iter().map(|(k, c)| CursorEntry::new(k, c)).collect(),
            traces: BTreeMap::from([(self.destination.clone(), self.traces.clone())]),
            records_sent: self.counters.records_sent,
            bytes_sent: self.counters.bytes_sent,
            started_unix: self.started_unix,
            updated_unix: now_nanos() / 1_000_000_000,
        };
        if let Ok(bytes) = serde_json::to_vec(&progress) {
            let path = progress_path(&self.data_dir);
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = crate::write_atomic(&path, &bytes);
        }
        self.last_write = Some(at);
    }

    /// The seconds left at the observed rate, from the bytes still unreplayed and the records
    /// they have averaged so far. `null` until there is anything to observe.
    fn eta_seconds(&self) -> Option<u64> {
        let elapsed = (now_nanos() / 1_000_000_000).saturating_sub(self.started_unix);
        if self.state != RunState::Running || elapsed == 0 || self.counters.records_sent == 0 {
            return None;
        }
        let remaining: u64 = self
            .files
            .iter()
            .map(|plan| {
                plan.until.saturating_sub(
                    self.cursors
                        .get(&tailer::key(&self.destination, &plan.file))
                        .map_or(0, |c| c.offset),
                )
            })
            .sum();
        let per_record = self.counters.records_built as f64 / self.counters.ledger_read.max(1) as f64;
        let rate = self.counters.records_sent as f64 / elapsed as f64;
        Some((remaining as f64 * per_record / rate).ceil() as u64)
    }

    /// Whether the run has ended (`state != "running"`).
    pub fn done(&self) -> bool {
        self.state != RunState::Running
    }

    /// Finish after the current tick: unacknowledged work is dropped, what was committed stays,
    /// and the next run replays the rest.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// The live view, for a caller that polls instead of waiting for [`Backfill::summary`].
    pub fn progress(&self) -> Value {
        serde_json::json!({
            "state": self.state.as_str(),
            "window": {"since": self.window.since, "until": self.window.until},
            "sources_done": self.sources_done(),
            "sources_total": self.files.len(),
            "records_sent": self.counters.records_sent,
            "bytes_sent": self.counters.bytes_sent,
            "eta_seconds": self.eta_seconds(),
            "dry_run": self.dry_run,
        })
    }

    /// The one-shot's answer, printed on stdout for the mothership: what was replayed, what was
    /// skipped, and how the run ended.
    pub fn summary(&self) -> Value {
        let signal = |name: &str| {
            let entry = self.counters.per_signal.get(name);
            serde_json::json!({
                "records": entry.map_or(0, |e| e.records),
                "ledger_bytes": entry.map_or(0, |e| e.ledger_bytes),
                "encoded_bytes": entry.map_or(0, |e| e.encoded_bytes),
            })
        };
        let mut per_org = serde_json::Map::new();
        for (org, entry) in &self.counters.per_org {
            per_org.insert(
                org.clone(),
                serde_json::json!({"records": entry.records, "ledger_bytes": entry.ledger_bytes}),
            );
        }
        serde_json::json!({
            "ok": self.state != RunState::Failed,
            "state": self.state.as_str(),
            "dry_run": self.dry_run,
            "window": {"since": self.window.since, "until": self.window.until},
            "repo": self.repo,
            "include_archive": self.include_archive,
            "archived_colonies": self.archive.len(),
            "sources_total": self.files.len(),
            "sources_done": self.sources_done(),
            "records_sent": self.counters.records_sent,
            "bytes_sent": self.counters.bytes_sent,
            "skipped": {
                "malformed": self.counters.malformed,
                "untimestamped": self.counters.untimestamped,
                "out_of_window": self.counters.out_of_window,
                "refused": self.counters.refused,
            },
            "per_signal": {"logs": signal("logs"), "traces": signal("traces")},
            "per_org": per_org,
            "eta_seconds": self.eta_seconds(),
            "error": self.error,
        })
    }
}

/// The one-shot: build the plan, drive ticks to completion, answer with the summary.
pub async fn run(contract_path: &Path, headers: Vec<(String, String)>, opts: &Options) -> Result<Value, String> {
    let mut backfill = Backfill::new(contract_path, headers, opts)?;
    while !backfill.done() {
        let wait = backfill.tick().await;
        tokio::time::sleep(wait).await;
    }
    Ok(backfill.summary())
}

#[cfg(test)]
mod tests;
