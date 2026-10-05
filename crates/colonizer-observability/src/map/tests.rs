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

fn digest(line: &Value) -> [u8; 32] {
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
