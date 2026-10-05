//! Ledger lines to OTLP: goldens for the log records and metric points a fixed set of lines maps
//! to, the record id formula, and the privacy rules (redaction again, content never read).
//! Regenerate the goldens with `UPDATE_GOLDEN=1 cargo test -p colonizer-observability ledger_`.

use super::*;
use crate::batch::{BatchConfig, Batcher};
use crate::encode::{Encoding, to_json, to_json_value};
use crate::metrics::Aggregates;
use crate::policy::{ContentGate, PolicyConfig};
use crate::testkit::{Canaries, assert_absent};
use serde_json::json;
use std::path::PathBuf;

const HOST: &str = "host-1";
const NOW: u64 = 1_790_000_000_000_000_000;

fn colonies() -> BTreeMap<String, ColonyPolicy> {
    BTreeMap::from([
        (
            "c0ffee12".to_string(),
            ColonyPolicy {
                org: "acme".into(),
                repo: "acme/widgets".into(),
                sensitivity: Some("standard".into()),
                status: Some("running".into()),
                ..ColonyPolicy::default()
            },
        ),
        (
            "d00d0001".to_string(),
            ColonyPolicy {
                org: "acme".into(),
                repo: "acme/gears".into(),
                status: Some("queued".into()),
                ..ColonyPolicy::default()
            },
        ),
    ])
}

/// One line of every mapped source, as the mothership writes them.
fn lines() -> Vec<(Source, Option<&'static str>, Value)> {
    vec![
        (
            Source::Harness,
            Some("c0ffee12"),
            json!({"type": "harness_log", "origin": "host", "level": "warn", "message": "quota paused for 60s", "ts": "2026-10-04T10:00:00Z"}),
        ),
        (
            Source::Events,
            Some("c0ffee12"),
            json!({"seq": 7, "type": "tool_call", "message_id": "m1", "tool_call_id": "toolu_1", "name": "Bash", "input": {"command": "cat secrets.txt"}, "ts": "2026-10-04T10:00:01.250Z"}),
        ),
        (
            Source::Events,
            Some("c0ffee12"),
            json!({"seq": 8, "type": "turn_end", "is_error": false, "result": "done", "cost_usd": 0.42, "duration_ms": 9100, "model_usage": {"claude-opus-4-5": {"input_tokens": 1200, "output_tokens": 340}}, "ts": "2026-10-04T10:00:10Z"}),
        ),
        (
            Source::Events,
            Some("c0ffee12"),
            json!({"seq": 9, "type": "assistant_text_delta", "delta": "hel", "ts": "2026-10-04T10:00:10Z"}),
        ),
        (
            Source::Gateway,
            Some("c0ffee12"),
            json!({"type": "gateway_request", "ts": "2026-10-04T10:00:02Z", "colony": "c0ffee12", "provider": "openrouter", "wire": "openai", "model": "qwen3-coder", "wire_model": "qwen/qwen3-coder", "method": "POST", "path": "/v1/chat/completions", "status": 429, "failure": "rate_limited", "fallback": true, "queue_ms": 30, "duration_ms": 812, "request_bytes": 2048, "response_bytes": 120, "input_tokens": null, "output_tokens": null}),
        ),
        (
            Source::Activity,
            None,
            json!({"seq": 3, "ts": "2026-10-04T10:00:03Z", "kind": "outcome.failed", "actor": "colony", "colony": "c0ffee12", "repo": "acme/widgets", "detail": "build broke at src/main.rs", "title": "Fix the widget"}),
        ),
        (
            Source::Spend,
            None,
            json!({"ts": "2026-10-04T10:00:04Z", "day": "2026-10-04", "org": "acme", "kind": "turn", "session": "c0ffee12", "agent": "claude-code", "model": "claude-opus-4-5", "input_tokens": 1200, "output_tokens": 340, "cache_read_tokens": 0, "cache_write_tokens": 0, "cost_usd": 0.42}),
        ),
    ]
}

fn policy() -> Policy {
    Policy::new(PolicyConfig::default(), ContentGate::closed(), None)
}

/// Stands in for the SHA-256 of the line's raw bytes. The keys are sorted first: a workspace build
/// can turn on serde_json's `preserve_order` (feature unification), and the `json!` text would then
/// keep source order and change the record ids in the golden.
fn digest(line: &Value) -> [u8; 32] {
    let mut line = line.clone();
    line.sort_all_objects();
    let d = ring::digest::digest(&ring::digest::SHA256, line.to_string().as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

fn map_all(policy: &Policy, lines: &[(Source, Option<&str>, Value)]) -> Vec<Item> {
    let colonies = colonies();
    let mapper = Mapper {
        policy,
        host_id: HOST,
        colonies: &colonies,
        now_unix_nanos: NOW,
    };
    lines
        .iter()
        .filter_map(|(source, colony, line)| mapper.log(*source, *colony, line, &digest(line)))
        .collect()
}

fn request(items: Vec<Item>) -> crate::encode::Request {
    let policy = policy();
    let resource = policy.resource(&[("service.name", "colonizer".into())]);
    let mut batcher = Batcher::new(
        &resource,
        BatchConfig {
            encoding: Encoding::Json,
            ..BatchConfig::default()
        },
    );
    for item in items {
        batcher.push(item);
    }
    let mut requests = batcher.finish().requests;
    assert_eq!(requests.len(), 1);
    requests.remove(0)
}

fn check_golden(name: &str, value: Value) {
    let version = format!(r#""version":"{}""#, env!("CARGO_PKG_VERSION"));
    let value: Value = serde_json::from_str(&value.to_string().replace(&version, r#""version":"<crate version>""#)).unwrap();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/otlp")
        .join(format!("{name}.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap() + "\n").unwrap();
    }
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}; regenerate with UPDATE_GOLDEN=1", path.display())),
    )
    .unwrap();
    assert_eq!(
        value, expected,
        "{name}: regenerate with UPDATE_GOLDEN=1 if the change is intended"
    );
}

#[test]
fn ledger_lines_map_to_the_golden_log_records() {
    let items = map_all(&policy(), &lines());
    assert_eq!(items.len(), 6, "every line but the text delta");
    check_golden("ledger-logs", to_json_value(&request(items)));
}

#[test]
fn ledger_lines_fold_into_the_golden_metric_points() {
    let mut agg = Aggregates {
        start_unix_nanos: NOW - 60_000_000_000,
        ..Aggregates::default()
    };
    for (source, _, line) in lines() {
        agg.fold(source, &line);
    }
    // A second, successful request of the same series.
    agg.fold(
        Source::Gateway,
        &json!({"provider": "openrouter", "model": "qwen3-coder", "status": 200, "failure": null, "queue_ms": 2, "duration_ms": 120, "input_tokens": 900, "output_tokens": 80}),
    );
    agg.exported = 6;
    agg.drop_count("backlog", 2);
    let points = agg.points(&policy(), &colonies(), NOW);
    check_golden("ledger-metrics", to_json_value(&request(points)));
}

#[test]
fn the_record_id_follows_the_design_formula() {
    // hex(sha256("colonizer.rec.v1|host-1|events|c0ffee12|7")[..16])
    let input = "colonizer.rec.v1|host-1|events|c0ffee12|7";
    let full = hex(ring::digest::digest(&ring::digest::SHA256, input.as_bytes()).as_ref());
    assert_eq!(record_id(HOST, Source::Events, "c0ffee12", "7"), full[..32]);
    // Replaying the same line yields the same id: the mapping is deterministic.
    let a = to_json(&request(map_all(&policy(), &lines())));
    let b = to_json(&request(map_all(&policy(), &lines())));
    assert_eq!(a, b);
}

#[test]
fn every_exported_string_is_redacted_again_and_content_is_never_read() {
    let canaries = Canaries::new();
    let mut ledger = Vec::new();
    for canary in &canaries.all {
        // In the one free-text field this slice exports (the harness message) and in fields it must
        // never read (tool output, activity detail).
        ledger.push((
            Source::Harness,
            Some("c0ffee12"),
            json!({"type": "harness_log", "origin": "host", "level": "info", "message": canary.text, "ts": "2026-10-04T10:00:00Z"}),
        ));
        ledger.push((
            Source::Events,
            Some("c0ffee12"),
            json!({"seq": 1, "type": "tool_result", "tool_call_id": "t", "output": canary.text, "is_error": true}),
        ));
        ledger.push((
            Source::Activity,
            None,
            json!({"seq": 1, "kind": "chat", "actor": "you", "detail": canary.text}),
        ));
    }
    ledger.push((
        Source::Events,
        Some("c0ffee12"),
        json!({"seq": 2, "type": "tool_call", "tool_call_id": "t", "name": "Bash", "input": {"command": canaries.content}}),
    ));
    ledger.push((
        Source::Events,
        Some("c0ffee12"),
        json!({"seq": 3, "type": "path_policy", "access": "write", "policy": "deny", "path": canaries.content, "tool": "Edit"}),
    ));

    for encoding in [Encoding::Json, Encoding::Protobuf] {
        let policy = policy();
        let resource = policy.resource(&[("service.name", "colonizer".into())]);
        let mut batcher = Batcher::new(
            &resource,
            BatchConfig {
                encoding,
                ..BatchConfig::default()
            },
        );
        for item in map_all(&policy, &ledger) {
            batcher.push(item);
        }
        for request in batcher.finish().requests {
            let bytes = request.encode(encoding);
            assert_absent(&bytes, canaries.secrets(), "ledger logs");
            assert_absent(&bytes, [canaries.content.as_str()], "content tier");
            if encoding == Encoding::Json {
                let text = String::from_utf8(bytes).unwrap();
                assert!(text.contains("[REDACTED:"), "redaction left its marks");
            }
        }
    }
}

#[test]
fn hashed_repo_names_never_leave_in_plain() {
    let root = std::env::temp_dir().join(format!("colonizer-map-hash-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&root).unwrap();
    let key = crate::hashing::HashKey::load_or_create(&root).unwrap();
    let policy = Policy::new(
        PolicyConfig {
            repo_names: crate::policy::RepoNames::Hashed,
            ..PolicyConfig::default()
        },
        ContentGate::closed(),
        Some(key),
    );
    let bytes = to_json(&request(map_all(&policy, &lines())));
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("acme/widgets"), "{text}");
    assert!(!text.contains("\"acme\""), "the org is hashed too: {text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// One line per kind family of every source the first slice did not map, as the mothership
/// writes them (findings from the v0.1.9 fixture), and the golden each maps to.
fn more_lines() -> Vec<(&'static str, Source, Option<&'static str>, Vec<Value>)> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../colonizer/tests/fixtures/data-v0.1.9/sessions/a1b2c3d4/findings.jsonl");
    let legacy: Value = serde_json::from_str(std::fs::read_to_string(fixture).unwrap().lines().next().unwrap()).unwrap();
    vec![
        (
            "logs-findings",
            Source::Findings,
            Some("c0ffee12"),
            vec![
                legacy,
                json!({"title": "token leaks into the log", "state": "filed", "ts": "2026-10-04T10:01:00Z", "severity": "high", "issue": "https://github.com/acme/widgets/issues/9", "reason": "the bearer is printed at debug"}),
                json!({"title": "token leaks into the log", "state": "fixed", "ts": "2026-10-04T11:00:00Z", "fix_session": "d00d0001", "pr": "https://github.com/acme/widgets/pull/10", "verdict": "confirmed"}),
            ],
        ),
        (
            "logs-decisions",
            Source::Decisions,
            None,
            vec![
                json!({"kind": "decision", "ts": "2026-10-04T10:02:00Z", "point": "recovery.retry", "session": "c0ffee12", "repo": "acme/widgets", "issue": 9, "mode": "act", "options": ["retry", "ask_human", "stop"], "pick": "retry", "confidence": 0.8, "latency_ms": 420, "miss": null, "did": "jev", "outcome": null}),
                json!({"kind": "outcome", "ts": "2026-10-04T10:32:00Z", "point": "recovery.retry", "session": "c0ffee12", "repo": "acme/widgets", "issue": 9, "mode": "act", "options": ["retry", "ask_human", "stop"], "pick": null, "confidence": null, "latency_ms": 0, "miss": "timeout", "did": "rule", "outcome": {"progressed": true, "window_min": 30}}),
            ],
        ),
        (
            "logs-routing",
            Source::Routing,
            None,
            vec![
                json!({"ts": "2026-10-04T10:00:00Z", "kind": "decision", "session": "c0ffee12", "repo": "acme/widgets", "issue": 9, "decision": {"point": "routing.tier", "jev_mode": "shadow", "jev_agrees": true, "floor": "low", "tier": "high", "rule": "high", "source": "rule", "score": 7, "reason": "touches src/auth.rs", "model": "claude-opus-4-5", "agent": "claude-code", "misroute": false, "signals": {"files": 3}, "jev": null, "cost": {"estimate_usd": 1.2}, "sensitivity": "standard"}}),
                json!({"ts": "2026-10-04T12:00:00Z", "kind": "actual", "session": "c0ffee12", "actual_cost_usd": 0.97}),
            ],
        ),
        (
            "logs-jev-ladder",
            Source::JevLadder,
            None,
            vec![
                json!({"kind": "decision", "ts": "2026-10-04T10:03:00Z", "session": "c0ffee12", "tool_call_id": "toolu_1", "tool": "Read", "action": "drop_result", "keep_call": 0.9, "keep_result": 0.2}),
                json!({"kind": "reread", "ts": "2026-10-04T10:04:00Z", "session": "c0ffee12", "tool_call_id": "toolu_7", "matched_tool_call_id": "toolu_1", "tool": "Read"}),
            ],
        ),
        (
            "logs-jev-focus",
            Source::JevFocus,
            None,
            vec![json!({"kind": "focus", "ts": "2026-10-04T10:05:00Z", "session": "c0ffee12", "mode": "shadow", "candidates": [{"label": "cargo test -p web", "files": 4}, {"label": "npm test", "files": 1}], "chosen": "cargo test -p web", "would_catch": true, "verdict": "confirmed", "actual_first_failure_ms": 81000, "focused_first_failure_ms": 12000, "total_ms": 95000, "checks_run": 2})],
        ),
        (
            "logs-mothership",
            Source::Mothership,
            None,
            vec![json!({"ts": "2026-10-04T10:06:00Z", "level": "warn", "target": "colonizer::queue", "message": "slot freed after 3 retries", "fields": {"colony": "c0ffee12"}})],
        ),
        (
            "logs-export-gap",
            Source::ExportGap,
            Some("c0ffee12"),
            vec![
                json!({"type": "export_gap", "ts": "2026-10-04T10:07:00Z", "reason": "backlog", "colony": "c0ffee12", "file": "sessions/c0ffee12/harness.jsonl", "bytes": 52_000, "lines": 48}),
                json!({"type": "export_gap", "ts": "2026-10-04T10:08:00Z", "reason": "deleted", "colony": "c0ffee12", "file": "sessions/c0ffee12", "archived": true}),
            ],
        ),
        (
            "logs-activity-summary",
            Source::Activity,
            None,
            vec![
                json!({"seq": 11, "ts": "2026-10-04T10:09:00Z", "kind": "decision.act", "actor": "colony", "colony": "c0ffee12", "repo": "acme/widgets", "detail": "recovery.retry: picked retry"}),
                json!({"seq": 12, "ts": "2026-10-04T10:10:00Z", "kind": "colony.answer", "actor": "you", "via": "cockpit", "colony": "c0ffee12", "detail": "use the staging database"}),
            ],
        ),
    ]
}

#[test]
fn every_remaining_source_maps_to_its_golden() {
    for (golden, source, colony, lines) in more_lines() {
        let input: Vec<_> = lines.into_iter().map(|line| (source, colony, line)).collect();
        let items = map_all(&policy(), &input);
        assert_eq!(items.len(), input.len(), "{golden}: one record per line");
        check_golden(golden, to_json_value(&request(items)));
    }
    // The content fields are never in a golden while the gate is closed.
    for golden in ["logs-findings", "logs-routing", "logs-activity-summary"] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/otlp/{golden}.json"));
        let text = std::fs::read_to_string(path).unwrap();
        for content in ["token leaks", "bearer is printed", "src/auth.rs", "staging database", "session writes bypass"] {
            assert!(!text.contains(content), "{golden} carries content: {content}");
        }
    }
}

#[test]
fn the_decision_and_jev_ledgers_key_their_records_on_the_row_session() {
    let line = json!({"kind": "decision", "ts": "2026-10-04T10:03:00Z", "session": "c0ffee12", "tool": "Read"});
    let item = map_all(&policy(), &[(Source::JevLadder, None, line.clone())]).remove(0);
    let json = to_json_value(&request(vec![item]));
    let attrs = &json["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["attributes"];
    let id = attrs
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["key"] == "colonizer.record.id")
        .unwrap()["value"]["stringValue"]
        .clone();
    assert_eq!(id, json!(record_id(HOST, Source::JevLadder, "c0ffee12", &hex(&digest(&line)))));
    assert!(attrs.to_string().contains("acme/widgets"), "the colony's repo joins in: {attrs}");
}

/// Every attribute key of a golden's log records, with the source it came from.
fn golden_keys(name: &str) -> Vec<(Source, String)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/otlp/{name}.json"));
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut out = Vec::new();
    for record in golden["resourceLogs"][0]["scopeLogs"][0]["logRecords"].as_array().unwrap() {
        let attrs = record["attributes"].as_array().unwrap();
        let source = attrs
            .iter()
            .find(|a| a["key"] == "colonizer.source")
            .and_then(|a| a["value"]["stringValue"].as_str())
            .unwrap();
        let source = Source::ALL.into_iter().find(|s| s.as_str() == source).unwrap();
        out.extend(attrs.iter().map(|a| (source, a["key"].as_str().unwrap().to_string())));
    }
    out
}

#[test]
fn no_golden_carries_a_key_outside_its_source_allowlist() {
    let mut names = vec!["ledger-logs"];
    names.extend(more_lines().iter().map(|(golden, ..)| *golden));
    let mut seen = std::collections::BTreeSet::new();
    // Keyed by name: `Source` is not ordered.
    for name in names {
        for (source, key) in golden_keys(name) {
            assert!(
                crate::policy::allowlist::lookup(&crate::policy::allowlist::for_source(source), &key).is_some(),
                "{name}: {key} is not allowlisted for {}",
                source.as_str()
            );
            seen.insert(source.as_str());
        }
    }
    // Every log source but the conversation's has a golden.
    assert_eq!(seen.len(), Source::ALL.len(), "{seen:?}");
}

#[test]
fn canaries_in_every_string_field_of_every_source_never_reach_the_wire() {
    let canaries = Canaries::new();
    let mut ledger = Vec::new();
    let mut inputs: Vec<(Source, Option<&str>, Value)> = lines();
    for (_, source, colony, lines) in more_lines() {
        inputs.extend(lines.into_iter().map(|l| (source, colony, l)));
    }
    /// Puts `text` into every string of `value`, nested ones included. Identifier keys skip only
    /// the redactor's high-entropy layer by design (an id must stay joinable): the `*_id` fields
    /// and the `colony`/`session` fields that become `colonizer.colony.id`. They get the canaries
    /// of known token shapes, not the opaque one.
    fn seed(value: &mut Value, text: &str, opaque: bool) {
        match value {
            Value::String(s) => *s = format!("{s} {text}"),
            Value::Array(items) => items.iter_mut().for_each(|v| seed(v, text, opaque)),
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    let id = key.ends_with("_id") || key == "colony" || key == "session";
                    if !(opaque && id) {
                        seed(v, text, opaque);
                    }
                }
            }
            _ => {}
        }
    }
    for canary in &canaries.all {
        for (source, colony, line) in &inputs {
            let mut line = line.clone();
            seed(&mut line, &canary.text, canary.name == "entropy");
            ledger.push((*source, *colony, line));
        }
    }
    // A reviewed kind's `detail` goes out as `summary`: redacted like any other string.
    for canary in &canaries.all {
        ledger.push((
            Source::Activity,
            None,
            json!({"seq": 1, "kind": "decision.act", "actor": "colony", "detail": canary.text}),
        ));
    }
    // The content canary in the `detail` of the kinds the gate holds back.
    for kind in ["colony.answer", "chat.colony", "outcome.question", "colonize.issue"] {
        ledger.push((
            Source::Activity,
            None,
            json!({"seq": 1, "kind": kind, "actor": "you", "detail": canaries.content}),
        ));
    }
    for encoding in [Encoding::Json, Encoding::Protobuf] {
        let policy = policy();
        let resource = policy.resource(&[("service.name", "colonizer".into())]);
        let mut batcher = Batcher::new(
            &resource,
            BatchConfig {
                encoding,
                ..BatchConfig::default()
            },
        );
        for item in map_all(&policy, &ledger) {
            batcher.push(item);
        }
        for request in batcher.finish().requests {
            let bytes = request.encode(encoding);
            assert_absent(&bytes, canaries.secrets(), "every source");
            assert_absent(&bytes, [canaries.content.as_str()], "activity detail, content tier");
        }
    }
}

#[test]
fn hashed_names_cover_the_remaining_sources() {
    let root = std::env::temp_dir().join(format!("colonizer-map-hash2-{}", crate::testkit::unique()));
    std::fs::create_dir_all(&root).unwrap();
    let key = crate::hashing::HashKey::load_or_create(&root).unwrap();
    let policy = Policy::new(
        PolicyConfig {
            repo_names: crate::policy::RepoNames::Hashed,
            ..PolicyConfig::default()
        },
        ContentGate::closed(),
        Some(key),
    );
    let mut input = Vec::new();
    for (_, source, colony, lines) in more_lines() {
        input.extend(lines.into_iter().map(|l| (source, colony, l)));
    }
    let text = String::from_utf8(to_json(&request(map_all(&policy, &input)))).unwrap();
    assert!(!text.contains("acme/widgets"), "{text}");
    assert!(!text.contains("\"acme\""), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Mapping and encoding 100 000 activity and gateway lines. The budget (≥ 50 000 records/s) holds
/// in a release build (about 105 000/s measured); a debug build is checked against 1 000/s.
#[test]
fn mapping_and_encoding_keep_up_with_a_busy_install() {
    let policy = policy();
    let colonies = colonies();
    let mapper = Mapper {
        policy: &policy,
        host_id: HOST,
        colonies: &colonies,
        now_unix_nanos: NOW,
    };
    let activity = json!({"seq": 3, "ts": "2026-10-04T10:00:03Z", "kind": "colony.launch", "actor": "you", "via": "api", "colony": "c0ffee12", "repo": "acme/widgets"});
    let gateway = json!({"type": "gateway_request", "ts": "2026-10-04T10:00:02Z", "provider": "openrouter", "wire": "openai", "model": "qwen3-coder", "status": 200, "queue_ms": 3, "duration_ms": 812, "request_bytes": 2048, "response_bytes": 900, "input_tokens": 1200, "output_tokens": 300});
    let (a, g) = (digest(&activity), digest(&gateway));
    let resource = policy.resource(&[("service.name", "colonizer".into())]);
    let n = 100_000;
    let started = std::time::Instant::now();
    let mut batcher = Batcher::new(&resource, BatchConfig::default());
    for i in 0..n {
        let item = if i % 2 == 0 {
            mapper.log(Source::Activity, None, &activity, &a)
        } else {
            mapper.log(Source::Gateway, Some("c0ffee12"), &gateway, &g)
        };
        batcher.push(item.unwrap());
    }
    let bytes: usize = batcher
        .finish()
        .requests
        .iter()
        .map(|r| r.encode(Encoding::Protobuf).len())
        .sum();
    let elapsed = started.elapsed();
    let rate = n as f64 / elapsed.as_secs_f64();
    eprintln!("mapped and encoded {n} records ({bytes} bytes) in {elapsed:?}: {rate:.0} records/s");
    let floor = if cfg!(debug_assertions) { 1_000.0 } else { 50_000.0 };
    assert!(rate >= floor, "{rate:.0} records/s is under {floor}");
}
