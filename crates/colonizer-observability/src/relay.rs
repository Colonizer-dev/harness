//! The fleet relay (#861): forwards the finished colonies members push to this owner
//! (`<data>/fleet-ingest/<member>/`, the mothership's `fleet_sync.rs`) to the owner's own OTLP
//! backend, so a fleet's observability lands in one place.
//!
//! Off unless `settings.relay_fleet_history` says so ([`Relay::new`] answers `None`). A row is
//! mapped exactly as the member's own exporter would map it — same policy pipeline, ids keyed on
//! its `origin_host`/`original_id`, `service.instance.id = origin_host` plus `colonizer.relayed_by`
//! on its resource, one batcher per member — under a colony policy whose content and thinking are
//! forced off whatever the contract says: the owner holds no opt-in from a member's orgs. A member
//! already streaming here itself (`relay_members`) is skipped, and a row's fingerprint — record
//! plus payload hashes — is kept in `<data>/observability/fleet-relay.json` only after this
//! backend accepts it; a pass it does not fully accept is rebuilt identically next tick, with rows
//! and lines capped per pass (record ids are deterministic, so a backend that dedupes absorbs any
//! overlap). Metrics — cumulative folds of ledgers the relay never sees — are never relayed; an
//! omitted or missing payload contributes nothing, and a missing ingest dir is a no-op, never an
//! error.

use crate::batch::{BatchConfig, Batcher, ExportResource, Item};
use crate::contract::{self, ColonyPolicy, Contract};
use crate::delivery::{Held, deliver, retry_delay};
use crate::encode::Request;
use crate::exporter::{self, policy_for};
use crate::map::{Mapper, hex};
use crate::policy::{Policy, Source};
use crate::sources;
use crate::traces::{Builder, Traces};
use crate::transport::Transport;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// The pause between passes while there is work; idle passes are far slower.
const TICK: Duration = Duration::from_secs(1);
/// The pause once a pass finds nothing to do: finished colonies arrive rarely.
const IDLE_TICK: Duration = Duration::from_secs(30);
/// The relay's state file, under `<data>/observability/`.
const STATE_FILE: &str = "fleet-relay.json";
/// The owner's per-member ingest root and rows file (`fleet_sync.rs`'s own names).
const INGEST_DIR: &str = "fleet-ingest";
const ROWS_FILE: &str = "sessions.json";
/// The most rows one pass starts, and the most lines one pass reads.
const MAX_ROWS_PER_PASS: usize = 16;
const MAX_LINES_PER_PASS: usize = 20_000;
/// The log payloads a row may reference, each with the source its lines map as, in the fixed order
/// a row is read in (`fleet_export`'s LOG_BASENAMES).
const PAYLOAD_SOURCES: [(&str, Source); 3] = [
    ("events.jsonl", Source::Events),
    ("gateway.jsonl", Source::Gateway),
    ("harness.jsonl", Source::Harness),
];

/// One payload a row references: `fleet_sync::PayloadRef` as it sits in `sessions.json`. Its
/// `bytes` is not read; unknown fields are ignored.
#[derive(Clone, Debug, Deserialize)]
struct PayloadRef {
    name: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    omitted: bool,
}

/// One stored row: the member's colony record and the payloads it arrived with. `received_at` is
/// not read — a re-upsert changes it without changing a byte of history.
#[derive(Clone, Debug, Deserialize)]
struct StoredRow {
    record: Value,
    #[serde(default)]
    payloads: Vec<PayloadRef>,
}

/// What became of the records handed to the backend, kept in the state file — the relay's only
/// diagnostics channel, as the exporter's own status is not its to write.
#[derive(Clone, Default, Serialize, Deserialize)]
struct Tally {
    sent: u64,
    rejected: u64,
    refused: u64,
}

/// `fleet-relay.json`: the rows the backend acknowledged, for one destination, and what became of
/// the records it was handed.
#[derive(Serialize, Deserialize)]
struct RelayState {
    version: u32,
    destination: String,
    acked: Vec<String>,
    #[serde(default)]
    tally: Tally,
}

/// A row mid-read: its fingerprint (a row whose fingerprint changed starts over) and the lines
/// already delivered.
type Progress = (String, usize);

/// One pass's work: the requests to send, and what is committed once every one is acknowledged.
struct Pass {
    requests: Vec<Request>,
    /// Row fingerprints the backend accepted.
    acked: Vec<String>,
    /// Row key (`<member>|<row id>`) → how far the row is read.
    progress: BTreeMap<String, Progress>,
    /// Per-member trace state; colony ids alone would collide across members.
    traces: BTreeMap<String, Traces>,
    /// Each member's rows file as of a fully read pass: unchanged, and with nothing partial, a
    /// member costs one stat instead of a parse.
    scan: BTreeMap<String, (u64, Option<SystemTime>)>,
}

/// The fleet relay, driven on its own task beside the live exporter (main.rs). Build with
/// [`Relay::new`]; `None` means this contract does not relay.
pub struct Relay {
    contract: Contract,
    data_dir: PathBuf,
    policy: Policy,
    transport: Transport,
    /// This backend's destination: the double-send guard compares a member's own against it.
    destination: String,
    /// Row fingerprints the backend has acknowledged.
    acked: BTreeSet<String>,
    /// Rows mid-read, their trace state, and each member's rows file as of its last full read —
    /// all adopted only by an accepted pass.
    progress: BTreeMap<String, Progress>,
    traces: BTreeMap<String, Traces>,
    scan: BTreeMap<String, (u64, Option<SystemTime>)>,
    tally: Tally,
    backoff: Option<(Instant, Duration)>,
}

impl Relay {
    /// `None` when the contract does not relay (`relay_fleet_history` off), speaks another version,
    /// or its transport refuses the settings: the relay is simply off, and the run loop carries on.
    pub fn new(contract_path: &Path, headers: Vec<(String, String)>) -> Option<Relay> {
        let contract = contract::load(contract_path).ok()?;
        if !contract.settings.relay_fleet_history {
            return None;
        }
        let own = env!("CARGO_PKG_VERSION");
        if !contract.mothership_version.is_empty() && contract.mothership_version != own {
            return None;
        }
        let transport = Transport::new(&contract.settings, headers).ok()?;
        // The exporter's own policy: the content gate closed, like every export this build makes.
        let policy = policy_for(&contract);
        let destination = exporter::destination_hash(&contract);
        let data_dir = contract.data_dir.clone();
        let (acked, tally): (BTreeSet<String>, Tally) = std::fs::read(state_path(&data_dir))
            .ok()
            .and_then(|b| serde_json::from_slice::<RelayState>(&b).ok())
            .filter(|s| s.version == 1 && s.destination == destination)
            .map(|s| (s.acked.into_iter().collect(), s.tally))
            .unwrap_or_default();
        Some(Relay {
            contract,
            data_dir,
            policy,
            transport,
            destination,
            acked,
            progress: BTreeMap::new(),
            traces: BTreeMap::new(),
            scan: BTreeMap::new(),
            tally,
            backoff: None,
        })
    }

    /// One pass: read what the caps allow from the ingest dir, send it through the shared
    /// [`deliver`](crate::delivery::deliver) loop, and commit the rows the backend acknowledged.
    /// Returns how long to wait before the next pass — fast while a backlog is still working
    /// down, slow once there is nothing to do.
    pub async fn tick(&mut self) -> Duration {
        if let Some((at, delay)) = self.backoff
            && at.elapsed() < delay
        {
            return delay - at.elapsed();
        }
        let pass = self.collect();
        let busy = !pass.requests.is_empty() || !pass.acked.is_empty();
        let mut requests = pass.requests;
        // Counted into the relay only with the pass that owns them; a held pass is rebuilt whole.
        let mut tally = self.tally.clone();
        match deliver(&self.transport, &mut requests, |ack| {
            tally.sent += ack.records.saturating_sub(ack.rejected);
            tally.rejected += ack.rejected;
        })
        .await
        {
            Ok(refused) => {
                self.backoff = None;
                let changed =
                    !pass.acked.is_empty() || pass.progress != self.progress || pass.traces != self.traces || refused > 0;
                tally.refused += refused;
                self.tally = tally;
                self.acked.extend(pass.acked);
                self.progress = pass.progress;
                self.traces = pass.traces;
                self.scan = pass.scan;
                if changed {
                    self.write_state();
                }
                if busy { TICK } else { IDLE_TICK }
            }
            // Nothing is committed: the next pass rebuilds the very same requests.
            Err(held) => {
                let after = match held {
                    Held::Retry(after) => after,
                    Held::Unauthorized(_) => None,
                };
                let delay = retry_delay(after, self.backoff.map(|(_, d)| d));
                self.backoff = Some((Instant::now(), delay));
                delay
            }
        }
    }

    /// Reads the ingest dir within this pass's caps and builds one batcher per member. The
    /// contract's stream switches gate a row's lines exactly as they gate the owner's own, and a
    /// gated line still counts as read, so its row completes. Nothing is committed here: the pass
    /// is adopted only after the backend accepts it.
    fn collect(&mut self) -> Pass {
        let now = exporter::now_nanos();
        let traces_on = self.contract.settings.stream_traces;
        let mut pass = Pass {
            requests: Vec::new(),
            acked: Vec::new(),
            progress: self.progress.clone(),
            traces: self.traces.clone(),
            scan: self.scan.clone(),
        };
        let mut items: BTreeMap<String, Vec<Item>> = BTreeMap::new();
        let Some(members) = members(&self.data_dir) else {
            return pass;
        };
        let mut rows_left = MAX_ROWS_PER_PASS;
        let mut lines_left = MAX_LINES_PER_PASS;
        'members: for member in members {
            let rows_file = self.data_dir.join(INGEST_DIR).join(&member).join(ROWS_FILE);
            let meta = std::fs::metadata(&rows_file).ok().map(|m| (m.len(), m.modified().ok()));
            let prefix = format!("{member}|");
            if meta.is_some_and(|m| self.scan.get(&member) == Some(&m))
                && !pass.progress.keys().any(|key| key.starts_with(&prefix))
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(&rows_file) else {
                continue;
            };
            let Ok(rows) = serde_json::from_slice::<BTreeMap<String, StoredRow>>(&bytes) else {
                continue;
            };
            // Set only when every row of this file was read to its end this pass.
            let mut complete = true;
            for (row_id, row) in rows {
                let text = |key: &str| row.record.get(key).and_then(Value::as_str).unwrap_or("");
                let (host, colony) = (text("origin_host"), text("original_id"));
                if host.is_empty() || colony.is_empty() {
                    continue;
                }
                let fingerprint = fingerprint(&row_id, &row);
                if self.acked.contains(&fingerprint) || self.relays_itself(host) {
                    continue;
                }
                if rows_left == 0 {
                    // Breaking out skips this member's scan entry: its unread rows must be read
                    // again next pass.
                    break 'members;
                }
                rows_left -= 1;
                let key = format!("{member}|{row_id}");
                // Lines already delivered in an accepted earlier pass — the payload files are
                // content-addressed, so the count resumes at the same line — unless the row's
                // fingerprint changed and it starts over.
                let mut consumed = match pass.progress.get(&key) {
                    Some((seen, lines)) if seen == &fingerprint => *lines,
                    _ => 0,
                };
                let mut seen = 0usize;
                let mut done = true;
                let colonies = BTreeMap::from([(colony.to_string(), colony_policy(&row.record))]);
                let mapper = Mapper {
                    policy: &self.policy,
                    host_id: host,
                    colonies: &colonies,
                    now_unix_nanos: now,
                };
                let builder = Builder {
                    policy: &self.policy,
                    host_id: host,
                    colonies: &colonies,
                    sample_ratio: self.contract.settings.trace_sample_ratio,
                    max_trace_bytes: self.contract.settings.max_trace_bytes,
                };
                'payloads: for (name, source) in PAYLOAD_SOURCES {
                    let Some(payload) = row.payloads.iter().find(|p| p.name == name) else {
                        continue;
                    };
                    let Some(bytes) = payload_bytes(&self.data_dir, &member, payload) else {
                        continue;
                    };
                    for line in bytes.split(|b| *b == b'\n') {
                        if line.is_empty() {
                            continue;
                        }
                        if seen < consumed {
                            seen += 1;
                            continue;
                        }
                        if lines_left == 0 {
                            done = false;
                            break 'payloads;
                        }
                        lines_left -= 1;
                        let digest = sha32(line);
                        if let Ok(value) = serde_json::from_slice::<Value>(line) {
                            let out = items.entry(host.to_string()).or_default();
                            if sources::logs_enabled(&self.contract.settings, source)
                                && let Some(item) = mapper.log(source, Some(colony), &value, &digest)
                            {
                                out.push(item);
                            }
                            if traces_on {
                                builder.feed(
                                    pass.traces.entry(host.to_string()).or_default(),
                                    source,
                                    Some(colony),
                                    &value,
                                    &digest,
                                    out,
                                );
                            }
                        }
                        seen += 1;
                        consumed += 1;
                    }
                }
                if done {
                    // The row's events are all read: any root span it leaves behind can go out.
                    let caught_up = |c: &str| c == colony;
                    builder.settle(
                        pass.traces.entry(host.to_string()).or_default(),
                        &caught_up,
                        &[],
                        now,
                        items.entry(host.to_string()).or_default(),
                    );
                    pass.progress.remove(&key);
                    pass.acked.push(fingerprint);
                } else {
                    complete = false;
                    pass.progress.insert(key, (fingerprint, consumed));
                }
            }
            if complete && let Some(meta) = meta {
                pass.scan.insert(member, meta);
            }
        }
        for (host, records) in items {
            let resource = self.resource(&host);
            let mut batcher = Batcher::new(
                &resource,
                BatchConfig {
                    encoding: self.transport.encoding(),
                    ..BatchConfig::default()
                },
            );
            for item in records {
                batcher.push(item);
            }
            pass.requests.extend(batcher.finish().requests);
        }
        pass
    }

    /// The member's resource: the owner's own ([`exporter::resource_for`]) with
    /// `service.instance.id` re-keyed to the member and `colonizer.relayed_by` added — both
    /// through the policy, like every string the relay sends.
    fn resource(&self, host: &str) -> ExportResource {
        let mut base = exporter::resource_for(&self.policy, &self.contract);
        let stamped = self.policy.resource(&[
            ("service.instance.id", host.into()),
            ("colonizer.relayed_by", self.contract.host_id.as_str().into()),
        ]);
        base.0.attributes.retain(|kv| kv.key != "service.instance.id");
        base.0.attributes.extend(stamped.0.attributes);
        base
    }

    /// Whether a member's own exporter already streams to this destination: the mothership fills
    /// `relay_members` from the member's fleet poll answer, and a member reporting `enabled` with
    /// this backend's hash has delivered its own rows. Anything else — a different destination, or
    /// not enabled — is the owner's to relay.
    fn relays_itself(&self, host: &str) -> bool {
        self.contract
            .relay_members
            .iter()
            .any(|m| m.host_id == host && m.enabled && m.destination_hash == self.destination)
    }

    /// Writes `fleet-relay.json` atomically: the destination the acked rows belong to, and what
    /// became of the records the backend was handed.
    fn write_state(&self) {
        let state = RelayState {
            version: 1,
            destination: self.destination.clone(),
            acked: self.acked.iter().cloned().collect(),
            tally: self.tally.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&state) {
            let path = state_path(&self.data_dir);
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = crate::write_atomic(&path, &bytes);
        }
    }
}

/// `<data>/observability/fleet-relay.json`.
fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join(STATE_FILE)
}

/// The member directories under the ingest root, sorted for a deterministic pass order. A missing
/// or unreadable root is `None`: nothing to relay, never an error.
fn members(data_dir: &Path) -> Option<Vec<String>> {
    let mut out: Vec<String> = std::fs::read_dir(data_dir.join(INGEST_DIR))
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    out.sort();
    Some(out)
}

/// A payload's stored bytes: `None` when the row lists it omitted, its hash is not the lowercase
/// hex a member writes (so a crafted name is never joined into a path), or its file is missing on
/// disk — the row's other payloads still go.
fn payload_bytes(data_dir: &Path, member: &str, payload: &PayloadRef) -> Option<Vec<u8>> {
    if payload.omitted || !is_sha256(&payload.sha256) {
        return None;
    }
    std::fs::read(data_dir.join(INGEST_DIR).join(member).join("payloads").join(&payload.sha256)).ok()
}

/// `true` for 64 characters of lowercase hex: the only shape ever joined into a payloads path.
fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A row's fingerprint: its id, its record and the payload hashes it arrived with — everything
/// except `received_at`, which a re-upsert changes without changing the history.
fn fingerprint(row_id: &str, row: &StoredRow) -> String {
    let mut input = format!("{row_id}|{}", serde_json::to_string(&row.record).unwrap_or_default());
    let mut refs = row.payloads.clone();
    refs.sort_by(|a, b| a.name.cmp(&b.name));
    for p in refs {
        input.push('|');
        input.push_str(&p.name);
        input.push(':');
        input.push_str(&p.sha256);
        if p.omitted {
            input.push_str(":omitted");
        }
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, input.as_bytes());
    hex(&digest.as_ref()[..16])
}

/// The colony policy a relayed row is mapped under: the record's own metadata, with content and
/// thinking forced off — an ingested row carries no sensitivity and the owner holds no opt-in from
/// the member's orgs, so the relay never opens them whatever the contract's own policies say.
fn colony_policy(record: &Value) -> ColonyPolicy {
    let text = |key: &str| {
        record
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    ColonyPolicy {
        org: text("org").unwrap_or_default(),
        repo: text("repo").unwrap_or_default(),
        status: text("status"),
        agent: text("agent").unwrap_or_default(),
        created_at: text("created_at"),
        pr_url: text("pr_url"),
        content: false,
        thinking: false,
        ..ColonyPolicy::default()
    }
}

/// The SHA-256 of a line's raw bytes, the digest its record id keys on.
fn sha32(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

#[cfg(test)]
mod tests;
