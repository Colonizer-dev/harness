//! The fleet relay (#861) against a fake OTLP collector: the off-by-default switch, the member's
//! identity and ids, the closed content gate under every opt-in, the double-send guard, and the
//! once-only state file.

use super::*;
use crate::contract::{RelayMember, Settings};
use crate::policy::Source;
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use prost::Message;
use std::io::Read;
use std::sync::{Arc, Mutex};

/// What the collector saw: the request's path and its body, decompressed.
type Seen = Vec<(String, Vec<u8>)>;

/// The fake collector: every accepted request, decompressed, by path.
struct Collector {
    url: String,
    seen: Arc<Mutex<Seen>>,
}

impl Collector {
    async fn start() -> Collector {
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let log = seen.clone();
        let handler = move |path: &'static str| {
            let log = log.clone();
            move |headers: HeaderMap, body: Bytes| {
                let log = log.clone();
                async move {
                    let mut raw = body.to_vec();
                    if headers.get("content-encoding").is_some_and(|v| v == "gzip") {
                        let mut out = Vec::new();
                        flate2::read::GzDecoder::new(raw.as_slice()).read_to_end(&mut out).unwrap();
                        raw = out;
                    }
                    log.lock().unwrap().push((path.to_string(), raw));
                    (StatusCode::OK, HeaderMap::new(), Vec::new())
                }
            }
        };
        let app = axum::Router::new()
            .route("/v1/logs", axum::routing::post(handler("/v1/logs")))
            .route("/v1/traces", axum::routing::post(handler("/v1/traces")));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Collector { url, seen }
    }

    fn bodies(&self) -> Vec<(String, Vec<u8>)> {
        self.seen.lock().unwrap().clone()
    }

    /// Every logs request the collector accepted, decoded.
    fn logs(&self) -> Vec<ExportLogsServiceRequest> {
        self.bodies()
            .iter()
            .filter(|(path, _)| path == "/v1/logs")
            .filter_map(|(_, body)| ExportLogsServiceRequest::decode(body.as_slice()).ok())
            .collect()
    }

    /// Every logs request's resource attribute, as `(key, value)` pairs.
    fn resource_attrs(&self) -> Vec<(String, String)> {
        self.logs()
            .into_iter()
            .flat_map(|r| r.resource_logs)
            .filter_map(|r| r.resource)
            .flat_map(|resource| resource.attributes)
            .filter_map(|kv| {
                let value = match kv.value?.value? {
                    crate::proto::common::v1::any_value::Value::StringValue(s) => s,
                    _ => return None,
                };
                Some((kv.key, value))
            })
            .collect()
    }

    /// Every accepted log record's `colonizer.record.id`.
    fn record_ids(&self) -> Vec<String> {
        self.logs()
            .into_iter()
            .flat_map(|r| r.resource_logs)
            .flat_map(|r| r.scope_logs)
            .flat_map(|s| s.log_records)
            .filter_map(|record| {
                let kv = record.attributes.into_iter().find(|kv| kv.key == "colonizer.record.id")?;
                match kv.value?.value? {
                    crate::proto::common::v1::any_value::Value::StringValue(s) => Some(s),
                    _ => None,
                }
            })
            .collect()
    }

    /// Every accepted span's trace id, as lowercase hex.
    fn trace_ids(&self) -> Vec<String> {
        self.bodies()
            .iter()
            .filter(|(path, _)| path == "/v1/traces")
            .filter_map(|(_, body)| ExportTraceServiceRequest::decode(body.as_slice()).ok())
            .flat_map(|r| r.resource_spans)
            .flat_map(|r| r.scope_spans)
            .flat_map(|s| s.spans)
            .map(|span| hex(&span.trace_id))
            .collect()
    }
}

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-relay-{tag}-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&dir).unwrap();
    Root(dir)
}

fn ts() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()).to_rfc3339()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// One member's ingest dir: `sessions.json` holding one row of `colony`, and its payloads by hash.
/// An entry with no lines is listed on the row as `omitted`, never uploaded; `summary` rides the
/// row's record, where free text the relay must never send would sit.
fn member(root: &Root, dir: &str, host: &str, colony: &str, payloads: &[(&str, Vec<Value>)], summary: &str) {
    let dir_path = root.0.join(INGEST_DIR).join(dir);
    let mut refs = Vec::new();
    for (name, lines) in payloads {
        if lines.is_empty() {
            refs.push(serde_json::json!({"name": name, "sha256": "", "bytes": 0, "omitted": true}));
            continue;
        }
        let mut bytes = Vec::new();
        for line in lines {
            bytes.extend_from_slice(serde_json::to_string(line).unwrap().as_bytes());
            bytes.push(b'\n');
        }
        let sha = sha256_hex(&bytes);
        std::fs::create_dir_all(dir_path.join("payloads")).unwrap();
        std::fs::write(dir_path.join("payloads").join(&sha), &bytes).unwrap();
        refs.push(serde_json::json!({"name": name, "sha256": sha, "bytes": bytes.len()}));
    }
    let rows = serde_json::json!({
        format!("{host}:{colony}"): {
            "record": {
                "id": format!("{host}:{colony}"), "origin_host": host, "original_id": colony,
                "repo": "acme/widgets", "org": "acme", "status": "merged", "agent": "claude_code",
                "created_at": "2026-10-01T10:00:00Z", "updated_at": "2026-10-02T10:00:00Z",
                "issue_title": "do the thing", "summary": summary,
            },
            "payloads": refs,
            "received_at": "2026-10-03T10:00:00Z",
        }
    });
    std::fs::create_dir_all(&dir_path).unwrap();
    std::fs::write(dir_path.join(ROWS_FILE), serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}

/// A member's typical logs: one turn on the events ledger, plus a gateway request the row lists as
/// an omitted payload (too large to travel) and a harness line.
fn turn_payloads() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        (
            "events.jsonl",
            vec![
                serde_json::json!({"type": "user_message", "seq": 1, "ts": ts()}),
                serde_json::json!({"type": "turn_end", "seq": 2, "ts": ts()}),
            ],
        ),
        ("gateway.jsonl", Vec::new()),
        (
            "harness.jsonl",
            vec![serde_json::json!({"type": "harness_log", "level": "info", "message": "turn done", "ts": ts()})],
        ),
    ]
}

/// Writes the contract for `url`; `edit` turns the relay on and fills whatever else a test needs.
fn contract(root: &Root, url: &str, edit: impl FnOnce(&mut Contract)) -> PathBuf {
    let mut c = Contract {
        contract: contract::CONTRACT,
        mothership_version: env!("CARGO_PKG_VERSION").into(),
        host_id: "host-1".into(),
        fleet_id: String::new(),
        data_dir: root.0.clone(),
        settings: Settings {
            endpoint: url.to_string(),
            stream_metrics: false,
            ..Settings::default()
        },
        policy: BTreeMap::new(),
        relay_members: Vec::new(),
    };
    edit(&mut c);
    let path = contract::path(&root.0);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    path
}

/// The relay's own state file, when a test wants to know it was written.
fn state_file(root: &Root) -> PathBuf {
    state_path(&root.0)
}

#[tokio::test]
async fn a_contract_without_the_setting_never_reads_or_sends() {
    let off = root("off");
    member(&off, "m1", "member-a", "col-1", &turn_payloads(), "done");
    let collector = Collector::start().await;
    let path = contract(&off, &collector.url, |_| {});
    assert!(
        Relay::new(&path, Vec::new()).is_none(),
        "the relay is off unless the contract turns it on"
    );
    assert!(collector.bodies().is_empty());

    // A contract that does turn it on, over a data dir with no ingest dir at all, is a quiet no-op.
    let bare = root("bare");
    let path = contract(&bare, &collector.url, |c| c.settings.relay_fleet_history = true);
    let mut relay = Relay::new(&path, Vec::new()).expect("the setting is on");
    relay.tick().await;
    assert!(collector.bodies().is_empty());
    assert!(!state_file(&bare).exists(), "nothing was acked, so nothing was written");
}

#[tokio::test]
async fn relayed_rows_carry_the_members_identity_and_ids() {
    let root = root("identity");
    member(&root, "m1", "member-a", "col-1", &turn_payloads(), "done");
    let collector = Collector::start().await;
    let path = contract(&root, &collector.url, |c| c.settings.relay_fleet_history = true);
    let mut relay = Relay::new(&path, Vec::new()).expect("the contract turns the relay on");
    relay.tick().await;

    let attrs = collector.resource_attrs();
    let attr = |key: &str| attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    assert_eq!(
        attr("service.instance.id").as_deref(),
        Some("member-a"),
        "the member's host, not the owner's"
    );
    assert_eq!(
        attr("colonizer.relayed_by").as_deref(),
        Some("host-1"),
        "the owner doing the forwarding"
    );

    // The ids equal what the member's own exporter computes, and differ from the owner's take.
    let trace = hex(&crate::traces::trace_id("member-a", "col-1"));
    assert!(collector.trace_ids().contains(&trace), "the member's trace id");
    let own = hex(&crate::traces::trace_id("host-1", "col-1"));
    assert!(!collector.trace_ids().contains(&own), "not the owner's trace id");
    let record = crate::map::record_id("member-a", Source::Events, "col-1", "1");
    assert!(
        collector.record_ids().contains(&record),
        "keyed by the member's host and colony"
    );
    assert!(
        !collector
            .record_ids()
            .contains(&crate::map::record_id("host-1", Source::Events, "col-1", "1"))
    );
    // The omitted gateway payload contributed nothing; the harness line still went out.
    assert!(
        collector.bodies().iter().any(|(path, _)| path == "/v1/traces"),
        "the turn became a span"
    );
    assert!(collector.bodies().len() >= 2, "logs and traces both went out");
}

#[tokio::test]
async fn content_never_rides_a_relayed_row_even_with_every_opt_in() {
    let root = root("canary");
    let canaries = crate::testkit::Canaries::new();
    let secret = &canaries.all[0];
    // Free text on the events ledger (no allowlisted key takes it) and in the row's own record:
    // with the contract's every content knob turned on — the org opted in, the colony's own policy
    // says content and thinking — the relay still synthesizes a closed policy per row.
    let payloads = vec![
        (
            "events.jsonl",
            vec![
                serde_json::json!({"type": "assistant_text", "seq": 1, "ts": ts(), "text": canaries.content}),
                serde_json::json!({"type": "turn_end", "seq": 2, "ts": ts()}),
            ],
        ),
        (
            "harness.jsonl",
            vec![serde_json::json!({"type": "harness_log", "level": "info", "message": "fine", "ts": ts()})],
        ),
    ];
    member(&root, "m1", "member-a", "col-1", &payloads, &secret.text);
    let collector = Collector::start().await;
    let path = contract(&root, &collector.url, |c| {
        c.settings.relay_fleet_history = true;
        c.policy.insert(
            "col-1".into(),
            crate::contract::ColonyPolicy {
                org: "acme".into(),
                repo: "acme/widgets".into(),
                content: true,
                thinking: true,
                ..crate::contract::ColonyPolicy::default()
            },
        );
    });
    let mut relay = Relay::new(&path, Vec::new()).unwrap();
    relay.tick().await;
    assert!(!collector.record_ids().is_empty(), "the row's structure is relayed");
    for (what, body) in collector.bodies() {
        crate::testkit::assert_absent(&body, [canaries.content.as_str()], &what);
        crate::testkit::assert_absent(&body, secret.secrets.iter().map(String::as_str), &what);
    }
}

#[tokio::test]
async fn a_member_streaming_here_itself_is_left_alone() {
    let root = root("double");
    member(&root, "m1", "member-a", "col-1", &turn_payloads(), "done");
    member(&root, "m2", "member-b", "col-2", &turn_payloads(), "done");
    member(&root, "m3", "member-c", "col-3", &turn_payloads(), "done");
    let collector = Collector::start().await;
    let path = contract(&root, &collector.url, |c| {
        c.settings.relay_fleet_history = true;
        let here = exporter::destination_hash(c);
        c.relay_members = vec![
            // Already streaming to exactly this backend.
            RelayMember {
                host_id: "member-a".into(),
                enabled: true,
                destination_hash: here.clone(),
            },
            // Streaming, but somewhere else.
            RelayMember {
                host_id: "member-b".into(),
                enabled: true,
                destination_hash: "deadbeef".into(),
            },
            // This backend, but its exporter is off.
            RelayMember {
                host_id: "member-c".into(),
                enabled: false,
                destination_hash: here,
            },
        ];
    });
    let mut relay = Relay::new(&path, Vec::new()).unwrap();
    relay.tick().await;
    let instances: Vec<String> = collector
        .resource_attrs()
        .into_iter()
        .filter(|(k, _)| k == "service.instance.id")
        .map(|(_, v)| v)
        .collect();
    assert!(
        !instances.contains(&"member-a".to_string()),
        "its own exporter already delivered it"
    );
    assert!(
        instances.contains(&"member-b".to_string()),
        "a different destination is the owner's to relay"
    );
    assert!(
        instances.contains(&"member-c".to_string()),
        "so is a member whose exporter is off"
    );
}

#[tokio::test]
async fn a_second_pass_and_a_restart_send_nothing_new() {
    let root = root("once");
    member(&root, "m1", "member-a", "col-1", &turn_payloads(), "done");
    let collector = Collector::start().await;
    let path = contract(&root, &collector.url, |c| c.settings.relay_fleet_history = true);
    let mut relay = Relay::new(&path, Vec::new()).unwrap();
    relay.tick().await;
    let first = collector.record_ids();
    assert!(!first.is_empty());
    assert!(state_file(&root).exists(), "the acked fingerprints are on disk");

    relay.tick().await;
    let mut again = Relay::new(&path, Vec::new()).unwrap();
    again.tick().await;
    assert_eq!(
        collector.record_ids(),
        first,
        "neither the same relay nor a restart re-sends a row"
    );
}

#[tokio::test]
async fn stream_switches_gate_what_a_relayed_row_sends() {
    // The contract's own switches, not just the member's, decide what a row carries: operational
    // logs off and traces off, the activity stream still goes and no span lane is even opened.
    // The gated lines still count as read, so the row completes rather than stalling.
    let switched = root("switches");
    member(&switched, "m1", "member-a", "col-1", &turn_payloads(), "done");
    let collector = Collector::start().await;
    let path = contract(&switched, &collector.url, |c| {
        c.settings.relay_fleet_history = true;
        c.settings.stream_operational = false;
        c.settings.stream_traces = false;
    });
    let mut relay = Relay::new(&path, Vec::new()).unwrap();
    relay.tick().await;
    assert!(
        collector.bodies().iter().all(|(path, _)| path == "/v1/logs"),
        "no span lane was opened"
    );
    assert!(collector.trace_ids().is_empty(), "no spans were built");
    assert!(!collector.record_ids().is_empty(), "the activity stream still goes");
    assert!(state_file(&switched).exists(), "the gated row was still read to its end");

    // The same row with every stream on sends strictly more: the harness line and the spans.
    let open = root("open");
    member(&open, "m1", "member-a", "col-1", &turn_payloads(), "done");
    let every = Collector::start().await;
    let path = contract(&open, &every.url, |c| c.settings.relay_fleet_history = true);
    Relay::new(&path, Vec::new()).unwrap().tick().await;
    assert!(!every.trace_ids().is_empty(), "the comparison run really streamed traces");
    assert!(
        collector.bodies().len() < every.bodies().len(),
        "the switches really gate records"
    );
}

#[tokio::test]
async fn a_payload_hash_that_is_not_hex_is_never_opened() {
    let root = root("sha");
    // 64 characters that would walk out of the payloads dir if they were ever joined into a
    // path; only lowercase hex ever is.
    let rows = serde_json::json!({
        "member-a:col-1": {
            "record": {
                "id": "member-a:col-1", "origin_host": "member-a", "original_id": "col-1",
                "repo": "acme/widgets", "org": "acme", "status": "merged", "agent": "claude_code",
            },
            "payloads": [{"name": "events.jsonl", "sha256": "../../".repeat(16), "bytes": 9}],
            "received_at": "2026-10-03T10:00:00Z",
        }
    });
    let dir = root.0.join(INGEST_DIR).join("m1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(ROWS_FILE), serde_json::to_vec(&rows).unwrap()).unwrap();
    let collector = Collector::start().await;
    let path = contract(&root, &collector.url, |c| c.settings.relay_fleet_history = true);
    let mut relay = Relay::new(&path, Vec::new()).unwrap();
    relay.tick().await;
    assert!(collector.bodies().is_empty(), "nothing was opened or sent");
    assert!(state_file(&root).exists(), "the row was still read to its end");
}
