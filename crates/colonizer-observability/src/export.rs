//! The offline export (#859): the same pipeline as the backfill — window, mapper, trace builder,
//! batcher — with a file for a backend instead of a transport. No network is built, nothing is
//! sent; every OTLP request is written as one OTLP/JSON line, so what an operator is about to ship
//! can be inspected, replayed and audited before it leaves the machine.
//!
//! - `--out` is a directory (or ends in `/`) for `logs.jsonl` and `traces.jsonl`, or one file every
//!   line goes into; the files are created `0600`, whatever the umask says.
//! - `--content` is P3's install switch. A colony's lines carry content only when the switch is
//!   set, the org opted in (`ColonyPolicy.content`) and the sensitivity is known and not
//!   restricted — and never for a colony the archive supplies, whose synthesized policy fails all
//!   three (fail closed). Spans are structure only regardless (P5).
//! - `--include-archive` reads the tar+zstd bundles of colonies the disk no longer holds, under
//!   the bundles' own host ids, exactly as the backfill does.

use crate::archive_source::{self, ArchivedColony, Message};
use crate::backfill::{MAX_LINE_BYTES, Window, flags};
use crate::batch::{BatchConfig, Batcher, Item};
use crate::contract::{self, Contract};
use crate::cursor::{Cursor, Limits, read_batch};
use crate::encode::{self, Encoding, Request};
use crate::exporter::{now_nanos, policy_for, policy_with_gate, resource_for};
use crate::map::{Mapper, ts_nanos};
use crate::policy::{ContentGate, Source};
use crate::sources::{self, SourceFile};
use crate::state::Signal;
use crate::traces::{Builder, Traces};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// How many records are held before the batcher is finished and its requests written: the export's
/// whole memory bound, since the batcher only answers at `finish()`.
const FLUSH_ITEMS: usize = crate::batch::DEFAULT_ITEMS;
/// One read's byte budget; the disk backfill spends this much a second, the export one per pass.
const READ_BYTES: u64 = 4 * 1024 * 1024;

/// What the operator asked for, parsed by [`Options::from_args`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// Unix nanos: export lines with `since <= ts` (inclusive), the backfill's grammar.
    pub since: Option<u64>,
    /// Unix nanos: export lines with `ts < until` (exclusive).
    pub until: Option<u64>,
    /// The output directory (or `dir/`) for `logs.jsonl` and `traces.jsonl`, or the one file.
    pub out: PathBuf,
    /// Whether log records and spans are exported (`--signals`, both by default).
    pub logs: bool,
    pub traces: bool,
    /// Also read the archive bundles of colonies the disk no longer holds.
    pub include_archive: bool,
    /// P3's install switch: content may leave the machine, per colony.
    pub content: bool,
}

impl Default for Options {
    /// Both signals, no window, no archive, no content: `--out` alone.
    fn default() -> Options {
        Options {
            since: None,
            until: None,
            out: PathBuf::new(),
            logs: true,
            traces: true,
            include_archive: false,
            content: false,
        }
    }
}

impl Options {
    /// Parses the `export` flags, skipping the `--contract <path>` pair the caller has already
    /// taken. An unknown flag, a missing value or an empty `--signals` is an error.
    pub fn from_args(args: &[String]) -> Result<Options, String> {
        let mut opts = Options {
            since: None,
            until: None,
            out: PathBuf::new(),
            logs: true,
            traces: true,
            include_archive: false,
            content: false,
        };
        let mut i = 0;
        while i < args.len() {
            let flag = args[i].as_str();
            if flag == "--include-archive" {
                opts.include_archive = true;
                i += 1;
                continue;
            }
            if flag == "--content" {
                opts.content = true;
                i += 1;
                continue;
            }
            let value = args.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?.clone();
            i += 2;
            match flag {
                // The contract path is the caller's business (every subcommand takes it).
                "--contract" => {}
                "--since" => opts.since = Some(flags::parse_when(&value, false)?),
                "--until" => opts.until = Some(flags::parse_when(&value, true)?),
                "--out" => opts.out = PathBuf::from(&value),
                "--signals" => {
                    let mut logs = false;
                    let mut traces = false;
                    for name in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        match name {
                            "logs" => logs = true,
                            "traces" => traces = true,
                            other => return Err(format!("--signals does not know {other}: logs, traces")),
                        }
                    }
                    if !logs && !traces {
                        return Err("--signals names nothing: logs, traces".to_string());
                    }
                    opts.logs = logs;
                    opts.traces = traces;
                }
                other => return Err(format!("unknown flag {other}")),
            }
        }
        if opts.out.as_os_str().is_empty() {
            return Err("--out is required: a directory or one file".to_string());
        }
        Ok(opts)
    }
}

/// The colonies whose lines may carry content under `--content` (P3): the org opted in
/// (`ColonyPolicy.content`), the sensitivity is known and not restricted, and the lines come off
/// the disk — a colony the archive supplies never qualifies, its synthesized policy fails all
/// three. Fail closed on everything else.
fn content_ok(opts: &Options, contract: &Contract, archive: &BTreeMap<String, ArchivedColony>) -> BTreeSet<String> {
    if !opts.content {
        return BTreeSet::new();
    }
    let mut ok: BTreeSet<String> = contract
        .policy
        .iter()
        .filter(|(_, p)| {
            p.content
                && p.sensitivity.as_deref().is_some_and(|s| {
                    matches!(
                        s.trim().to_ascii_lowercase().as_str(),
                        "open" | "standard" | "custom" | "vetted"
                    )
                })
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in archive.keys() {
        ok.remove(id);
    }
    ok
}

/// The colony a line belongs to, the way `map.rs` attributes one: the file's own colony, else the
/// line's `colony` (activity) or `session` (spend, decisions, routing, the Jev ledgers).
fn line_colony<'a>(file_colony: Option<&'a str>, line: &'a Value) -> Option<&'a str> {
    file_colony.or_else(|| line.get("colony").or_else(|| line.get("session")).and_then(Value::as_str))
}

/// The export's tally, in memory.
struct Counters {
    requests: u64,
    logs: u64,
    spans: u64,
    /// Every colony whose lines were kept, disk and archive together.
    colonies: BTreeSet<String>,
}

/// Where the requests go: the files, opened `0600`, one per signal in directory mode or one for
/// both in file mode. Requests are written as they are finished, so nothing accumulates.
struct Sink {
    files: Vec<std::io::BufWriter<std::fs::File>>,
    paths: Vec<PathBuf>,
    logs: Option<usize>,
    traces: Option<usize>,
}

impl Sink {
    /// Directory mode takes `logs.jsonl` and `traces.jsonl` per requested signal; file mode takes
    /// one file for everything.
    fn new(out: &Path, logs: bool, traces: bool) -> Result<Sink, String> {
        let mut sink = Sink {
            files: Vec::new(),
            paths: Vec::new(),
            logs: None,
            traces: None,
        };
        if out.is_dir() || out.as_os_str().to_string_lossy().ends_with('/') {
            std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
            if logs {
                sink.logs = Some(sink.push(out.join("logs.jsonl"))?);
            }
            if traces {
                sink.traces = Some(sink.push(out.join("traces.jsonl"))?);
            }
        } else {
            if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            if logs || traces {
                let at = sink.push(out.to_path_buf())?;
                if logs {
                    sink.logs = Some(at);
                }
                if traces {
                    sink.traces = Some(at);
                }
            }
        }
        Ok(sink)
    }

    fn push(&mut self, path: PathBuf) -> Result<usize, String> {
        let at = self.files.len();
        self.files.push(std::io::BufWriter::new(
            create_private(&path).map_err(|e| format!("cannot write {}: {e}", path.display()))?,
        ));
        self.paths.push(path);
        Ok(at)
    }

    /// One request as one OTLP/JSON line, in the signal's file.
    fn write(&mut self, request: &Request, counters: &mut Counters) -> Result<(), String> {
        let at = match request {
            Request::Logs(_) => self.logs,
            Request::Traces(_) => self.traces,
            // Metrics are never built here.
            Request::Metrics(_) => None,
        };
        if let Some(at) = at
            && let Some(file) = self.files.get_mut(at)
        {
            file.write_all(&encode::to_json(request))
                .and_then(|()| file.write_all(b"\n"))
                .map_err(|e| format!("cannot write the export: {e}"))?;
        }
        counters.requests += 1;
        match request {
            Request::Logs(_) => counters.logs += request.len() as u64,
            Request::Traces(_) => counters.spans += request.len() as u64,
            Request::Metrics(_) => {}
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        for mut file in self.files.drain(..) {
            file.flush().map_err(|e| format!("cannot write the export: {e}"))?;
        }
        Ok(())
    }
}

/// The output file, created private (0600) however the path arrived: the requests it holds are the
/// mothership's own ledgers, and a umask must not loosen that.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// The export: build the same records the backfill would send, write them as OTLP/JSON lines, and
/// answer with the one-line summary the caller prints. Fully synchronous — the binary runs it on a
/// blocking thread — and bounded: one read's lines, one batch, one chunk of the archive at a time.
pub fn run(contract_path: &Path, opts: &Options) -> Result<Value, String> {
    let contract = contract::load(contract_path)?;
    let own = env!("CARGO_PKG_VERSION");
    if !contract.mothership_version.is_empty() && contract.mothership_version != own {
        return Err(format!(
            "refused: the mothership is {}, this add-on is {own}; install the matching add-on",
            contract.mothership_version
        ));
    }
    let mut sink = Sink::new(&opts.out, opts.logs, opts.traces)?;
    let mut counters = Counters {
        requests: 0,
        logs: 0,
        spans: 0,
        colonies: BTreeSet::new(),
    };
    let mut malformed = 0u64;

    // The closed policy builds every record; the open one, only a content-ok colony's log records.
    let closed = policy_for(&contract);
    let open = opts.content.then(|| policy_with_gate(&contract, ContentGate::open()));
    let resource = resource_for(&closed, &contract);

    // The archive colonies, and the one colony list the mappers and builders see: contract policies
    // with the archived ones' fail-closed synthesis riding on top. Disk wins — a colony with a live
    // session directory is never read out of a bundle.
    let data_dir = contract.data_dir.clone();
    let archive = if opts.include_archive {
        let on_disk: BTreeSet<String> = contract
            .policy
            .keys()
            .filter(|id| data_dir.join("sessions").join(id).is_dir())
            .cloned()
            .collect();
        archive_source::discover(&data_dir, None, &on_disk)
    } else {
        BTreeMap::new()
    };
    let content_ok = content_ok(opts, &contract, &archive);
    let mut colonies_map = contract.policy.clone();
    for archived in archive.values() {
        colonies_map.insert(archived.colony.clone(), archived.policy.clone());
    }
    let archive_hosts: BTreeMap<&str, &str> = archive.values().map(|a| (a.colony.as_str(), a.host.as_str())).collect();

    let window = Window {
        since: opts.since,
        until: opts.until,
    };
    let settings = &contract.settings;
    let now = now_nanos();

    // One pipeline end: the batcher is finished and written every FLUSH_ITEMS records, so the run
    // holds one batch at most.
    let mut batcher = Batcher::new(
        &resource,
        BatchConfig {
            encoding: Encoding::Json,
            ..BatchConfig::default()
        },
    );
    let mut in_flight = 0usize;
    let flush = |batcher: &mut Batcher, in_flight: &mut usize, sink: &mut Sink, counters: &mut Counters| -> Result<(), String> {
        let done = std::mem::replace(
            batcher,
            Batcher::new(
                &resource,
                BatchConfig {
                    encoding: Encoding::Json,
                    ..BatchConfig::default()
                },
            ),
        );
        for request in done.finish().requests {
            sink.write(&request, counters)?;
        }
        *in_flight = 0;
        Ok(())
    };
    let push = |item: Item, batcher: &mut Batcher, in_flight: &mut usize| {
        batcher.push(item);
        *in_flight += 1;
    };

    // The disk half: every logs and traces source of the contract's colonies, whole files, in a
    // deterministic order.
    let colonies: Vec<String> = contract.policy.keys().cloned().collect();
    let mut files: Vec<SourceFile> = sources::discover(&data_dir, settings, &colonies)
        .into_iter()
        .filter(|file| match file.signal {
            Signal::Logs => opts.logs,
            Signal::Traces => opts.traces,
            Signal::Metrics => false,
        })
        .collect();
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    let mut traces = Traces::default();
    let mut fleet: BTreeMap<String, Traces> = BTreeMap::new();
    let limits = Limits {
        max_bytes: READ_BYTES,
        max_lines: FLUSH_ITEMS,
        max_line_bytes: MAX_LINE_BYTES,
    };
    for file in &files {
        if !file.live.exists() && file.rolled.as_deref().is_none_or(|p| !p.exists()) {
            continue; // a source that is only configured holds no records
        }
        let mut cursor = Cursor::default();
        while let Ok(batch) = read_batch(&file.live, file.rolled.as_deref(), &cursor, &limits) {
            let advanced = batch.cursor.offset > cursor.offset;
            malformed += batch.malformed;
            for (line, digest) in batch.lines.iter().zip(&batch.digests) {
                if window.bounded() {
                    match ts_nanos(line) {
                        Some(ts) if window.keeps(ts) => {}
                        // A line with no ts cannot be placed in a window.
                        None => continue,
                        Some(_) => continue,
                    }
                }
                let colony = line_colony(file.colony.as_deref(), line);
                // The content gate is per line: one colony's opt-in never opens another's.
                let policy = if colony.is_some_and(|c| content_ok.contains(c)) {
                    open.as_ref().unwrap_or(&closed)
                } else {
                    &closed
                };
                let mut spans = Vec::new();
                match file.signal {
                    Signal::Logs => {
                        let mapper = Mapper {
                            policy,
                            host_id: &contract.host_id,
                            colonies: &colonies_map,
                            now_unix_nanos: now,
                        };
                        if let Some(item) = mapper.log(file.source, file.colony.as_deref(), line, digest) {
                            if let Some(colony) = colony {
                                counters.colonies.insert(colony.to_string());
                            }
                            push(item, &mut batcher, &mut in_flight);
                        }
                    }
                    Signal::Traces => {
                        // Spans are structure only (P5); an activity outcome for a colony another
                        // host archived closes that host's root span, under that host's ids.
                        let other_host: Option<&str> = if file.source == Source::Activity {
                            line.get("colony")
                                .and_then(Value::as_str)
                                .and_then(|c| archive_hosts.get(c).copied())
                        } else {
                            None
                        };
                        let builder = Builder {
                            policy: &closed,
                            host_id: other_host.unwrap_or(contract.host_id.as_str()),
                            colonies: &colonies_map,
                            sample_ratio: settings.trace_sample_ratio,
                            max_trace_bytes: settings.max_trace_bytes,
                        };
                        let state = match other_host {
                            Some(host) => fleet.entry(host.to_string()).or_default(),
                            None => &mut traces,
                        };
                        builder.feed(state, file.source, file.colony.as_deref(), line, digest, &mut spans);
                        if let Some(colony) = colony {
                            counters.colonies.insert(colony.to_string());
                        }
                    }
                    Signal::Metrics => {}
                }
                for item in spans {
                    push(item, &mut batcher, &mut in_flight);
                }
                if in_flight >= FLUSH_ITEMS {
                    flush(&mut batcher, &mut in_flight, &mut sink, &mut counters)?;
                }
            }
            cursor = batch.cursor.clone();
            if !advanced {
                break; // the file's end
            }
        }
    }

    // The archive half: whole bundles through the same mapping, under the bundles' host ids. The
    // blocking receive is the backpressure — the reader holds a few chunks, no more.
    if !archive.is_empty() {
        let (tx, rx) = archive_source::channel();
        let bundles: Vec<_> = archive.values().cloned().collect();
        let reader = std::thread::Builder::new()
            .name("observability-archive".into())
            .spawn(move || archive_source::stream(&bundles, tx))
            .map_err(|e| format!("cannot start the archive reader: {e}"))?;
        for message in rx {
            match message {
                Message::Lines { colony, host, lines } => {
                    let policy = if content_ok.contains(&colony) {
                        open.as_ref().unwrap_or(&closed)
                    } else {
                        &closed
                    };
                    for line in lines {
                        if window.bounded() {
                            match ts_nanos(&line.line) {
                                Some(ts) if window.keeps(ts) => {}
                                None => continue,
                                Some(_) => continue,
                            }
                        }
                        if opts.logs && sources::logs_enabled(settings, line.source) {
                            let mapper = Mapper {
                                policy,
                                host_id: &host,
                                colonies: &colonies_map,
                                now_unix_nanos: now,
                            };
                            if let Some(item) = mapper.log(line.source, Some(&colony), &line.line, &line.digest) {
                                counters.colonies.insert(colony.clone());
                                push(item, &mut batcher, &mut in_flight);
                            }
                        }
                        if opts.traces && matches!(line.source, Source::Events | Source::Gateway) && settings.stream_traces {
                            let builder = Builder {
                                policy: &closed,
                                host_id: &host,
                                colonies: &colonies_map,
                                sample_ratio: settings.trace_sample_ratio,
                                max_trace_bytes: settings.max_trace_bytes,
                            };
                            // A colony this very host archived shares its trace state with the disk
                            // half; another host's is the fleet map's, under that host's ids.
                            let state = if host == contract.host_id {
                                &mut traces
                            } else {
                                fleet.entry(host.clone()).or_default()
                            };
                            let mut spans = Vec::new();
                            builder.feed(state, line.source, Some(&colony), &line.line, &line.digest, &mut spans);
                            counters.colonies.insert(colony.clone());
                            for item in spans {
                                push(item, &mut batcher, &mut in_flight);
                            }
                        }
                        if in_flight >= FLUSH_ITEMS {
                            flush(&mut batcher, &mut in_flight, &mut sink, &mut counters)?;
                        }
                    }
                }
                Message::Done { malformed: bad } => malformed += bad,
            }
        }
        let _ = reader.join();
    }

    // The roots: every file and bundle is fully read, so every colony is caught up.
    if opts.traces && settings.stream_traces {
        let settle = |host: &str, state: &mut Traces| {
            let builder = Builder {
                policy: &closed,
                host_id: host,
                colonies: &colonies_map,
                sample_ratio: settings.trace_sample_ratio,
                max_trace_bytes: settings.max_trace_bytes,
            };
            let mut spans = Vec::new();
            builder.settle(state, &|_| true, &[], now, &mut spans);
            spans
        };
        for item in settle(&contract.host_id, &mut traces) {
            push(item, &mut batcher, &mut in_flight);
        }
        for (host, state) in fleet.iter_mut() {
            for item in settle(host, state) {
                push(item, &mut batcher, &mut in_flight);
            }
        }
    }
    flush(&mut batcher, &mut in_flight, &mut sink, &mut counters)?;
    sink.finish()?;

    Ok(serde_json::json!({
        "files": sink.paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "requests": counters.requests,
        "logs": counters.logs,
        "spans": counters.spans,
        "colonies": counters.colonies.len(),
        "archived": archive.len(),
        "skipped": {"malformed": malformed},
    }))
}

#[cfg(test)]
mod tests;
