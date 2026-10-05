use super::*;
use crate::policy::{ContentGate, Policy, PolicyConfig, Source, SpanKind, Tier};
use serde_json::{Map, Value};

fn policy() -> Policy {
    let config = PolicyConfig {
        max_content_bytes: 196_608,
        ..PolicyConfig::default()
    };
    Policy::new(config, ContentGate::closed(), None)
}

/// Words, not a payload or a secret: `n` bytes of them.
fn prose(n: usize) -> String {
    "the quick brown fox jumps over a lazy dog ".repeat(n / 42 + 1)[..n].to_string()
}

/// Logs, spans and metrics of varied sizes, in an interleaved order.
fn mixed(policy: &Policy, n: usize) -> Vec<Item> {
    (0..n)
        .map(|i| match i % 3 {
            0 => policy
                .log(Source::Harness)
                .time(i as u64)
                .attr("level", "info", Tier::Structure)
                .body(&prose(i * 37 % 3000), Tier::Structure)
                .finish(),
            1 => policy
                .span(SpanKind::ExecuteTool, "Bash")
                .ids([7; 16], (i as u64).to_be_bytes(), None)
                .times(i as u64, i as u64 + 5)
                .attr("tool.name", "Bash", Tier::Structure)
                .finish(),
            _ => policy
                .metric(Source::Gateway, "colonizer.gateway.requests", "1")
                .sum(true)
                .times(0, i as u64)
                .attr("model", format!("model-{}", i % 17), Tier::Structure)
                .int(i as i64)
                .finish(),
        })
        .collect()
}

fn run(items: Vec<Item>, config: BatchConfig) -> Batch {
    let mut batcher = Batcher::new(&policy().resource(&[("service.name", "colonizer".into())]), config);
    items.into_iter().for_each(|i| batcher.push(i));
    batcher.finish()
}

fn scope(s: &Option<InstrumentationScope>) -> (&str, &str) {
    s.as_ref().map(|s| (s.name.as_str(), s.version.as_str())).expect("a scope")
}

fn scopes(request: &Request) -> Vec<(&str, &str)> {
    match request {
        Request::Logs(r) => r
            .resource_logs
            .iter()
            .flat_map(|r| &r.scope_logs)
            .map(|s| scope(&s.scope))
            .collect(),
        Request::Traces(r) => r
            .resource_spans
            .iter()
            .flat_map(|r| &r.scope_spans)
            .map(|s| scope(&s.scope))
            .collect(),
        Request::Metrics(r) => r
            .resource_metrics
            .iter()
            .flat_map(|r| &r.scope_metrics)
            .map(|s| scope(&s.scope))
            .collect(),
    }
}

#[test]
fn ten_thousand_mixed_items_fit_their_budgets_in_both_encodings() {
    for encoding in [Encoding::Protobuf, Encoding::Json] {
        for (max_request_bytes, max_items) in [(65_536, 500), (DEFAULT_REQUEST_BYTES, DEFAULT_ITEMS)] {
            let config = BatchConfig {
                max_request_bytes,
                max_items,
                encoding,
            };
            let batch = run(mixed(&policy(), 10_000), config);
            assert_eq!((batch.oversized, batch.truncated), (0, 0));
            assert_eq!(batch.requests.iter().map(Request::len).sum::<usize>(), 10_000);
            let mut per_path = std::collections::BTreeMap::new();
            for request in &batch.requests {
                let bytes = request.encode(encoding);
                assert_eq!(bytes.len(), request.encoded_len(encoding));
                assert!(
                    bytes.len() <= max_request_bytes,
                    "{encoding:?}: {} > {max_request_bytes}",
                    bytes.len()
                );
                assert!(request.len() <= max_items && !request.is_empty());
                assert_eq!(scopes(request), [("colonizer", env!("CARGO_PKG_VERSION"))]);
                per_path
                    .entry(request.path())
                    .or_insert_with(Vec::new)
                    .push((bytes.len(), request.len()));
            }
            // Requests are packed, not merely bounded: each but a lane's last is nearly full.
            for (path, sizes) in &per_path {
                for (bytes, items) in &sizes[..sizes.len() - 1] {
                    assert!(
                        *items == max_items || *bytes > max_request_bytes - 4096,
                        "{encoding:?} {path}: {bytes} bytes, {items} items"
                    );
                }
            }
            assert_eq!(per_path.len(), 3);
        }
    }
}

#[test]
fn sizing_is_exact_in_json_and_bounded_in_protobuf() {
    let p = policy();
    let resource = p.resource(&[("service.name", "colonizer".into())]).0;
    let scope = InstrumentationScope {
        name: "colonizer".into(),
        ..InstrumentationScope::default()
    };
    let logs: Vec<LogRecord> = mixed(&p, 300)
        .into_iter()
        .filter_map(|i| match i.0 {
            Record::Log(r) => Some(r),
            _ => None,
        })
        .collect();
    for encoding in [Encoding::Json, Encoding::Protobuf] {
        let lane = Lane::<LogRecord>::new(&resource, &scope, encoding);
        let parts: usize = logs.iter().enumerate().map(|(i, r)| Lane::part(r, encoding, i == 0)).sum();
        let actual = LogRecord::request(resource.clone(), scope.clone(), logs.clone()).encoded_len(encoding);
        match encoding {
            Encoding::Json => assert_eq!(actual, lane.envelope + parts),
            Encoding::Protobuf => assert!(actual <= lane.envelope + parts && actual + PROTOBUF_SLACK >= lane.envelope + parts),
        }
    }
}

#[test]
fn an_oversized_item_is_truncated_to_fit_not_dropped() {
    let p = policy();
    // Quotes double in JSON, so the first cut is not enough there; the loop goes on.
    for body in [prose(100_000), "\"quoted\" ".repeat(10_000)] {
        for encoding in [Encoding::Protobuf, Encoding::Json] {
            let config = BatchConfig {
                max_request_bytes: 8192,
                max_items: 10,
                encoding,
            };
            let item = p
                .log(Source::Harness)
                .attr("level", "warn", Tier::Structure)
                .body(&body, Tier::Structure)
                .finish();
            let batch = run(vec![item], config);
            assert_eq!((batch.oversized, batch.truncated, batch.requests.len()), (0, 1, 1));
            let request = &batch.requests[0];
            assert!(request.encoded_len(encoding) <= 8192);
            let Request::Logs(r) = request else { panic!("logs") };
            let record = &r.resource_logs[0].scope_logs[0].log_records[0];
            let Some(any_value::Value::StringValue(text)) = record.body.as_ref().and_then(|b| b.value.clone()) else {
                panic!("text body")
            };
            assert!(
                text.ends_with(" more bytes)") && text.len() > 4096,
                "{encoding:?}: {} bytes",
                text.len()
            );
            let flag = record.attributes.iter().filter(|kv| kv.key == TRUNCATED).count();
            assert_eq!(flag, 1, "the policy's own flag, once");
        }
    }
}

#[test]
fn an_item_that_cannot_be_cut_to_fit_is_dropped_and_counted() {
    let p = policy();
    // Thousands of short strings: no one string can be cut far enough.
    let body: Map<String, Value> = (0..2500)
        .map(|i| (format!("k{i:04}"), Value::from(format!("value {i:04} of a list"))))
        .collect();
    for encoding in [Encoding::Protobuf, Encoding::Json] {
        let config = BatchConfig {
            max_request_bytes: 8192,
            encoding,
            ..BatchConfig::default()
        };
        let items = vec![
            p.log(Source::Events)
                .body_json(&Value::Object(body.clone()), Tier::Structure)
                .finish(),
            p.log(Source::Harness).body("after it", Tier::Structure).finish(),
        ];
        let batch = run(items, config);
        assert_eq!((batch.oversized, batch.truncated), (1, 0));
        assert_eq!(
            batch.requests.iter().map(Request::len).sum::<usize>(),
            1,
            "the next record still goes"
        );
    }
}

#[test]
fn an_over_budget_request_is_halved_until_it_fits() {
    let p = policy();
    let resource = p.resource(&[]).0;
    let scope = InstrumentationScope::default();
    let logs: Vec<LogRecord> = (0..10)
        .map(
            |_| match p.log(Source::Harness).body(&prose(2000), Tier::Structure).finish().0 {
                Record::Log(r) => r,
                _ => unreachable!(),
            },
        )
        .collect();
    let mut batch = Batch::default();
    let mut ctx = Ctx {
        resource: &resource,
        scope: &scope,
        config: BatchConfig {
            max_request_bytes: 8192,
            ..BatchConfig::default()
        },
        batch: &mut batch,
    };
    emit(LogRecord::request(resource.clone(), scope.clone(), logs), &mut ctx);
    assert!(batch.requests.len() >= 3);
    assert!(batch.requests.iter().all(|r| r.encoded_len(Encoding::Protobuf) <= 8192));
    assert_eq!(batch.requests.iter().map(Request::len).sum::<usize>(), 10);
}

#[test]
fn config_is_clamped() {
    let wild = BatchConfig {
        max_request_bytes: usize::MAX,
        max_items: 0,
        encoding: Encoding::Json,
    };
    assert_eq!(wild.clamped().max_request_bytes, MAX_REQUEST_BYTES);
    assert_eq!(wild.clamped().max_items, 1);
    let tiny = BatchConfig {
        max_request_bytes: 1,
        ..BatchConfig::default()
    };
    assert_eq!(tiny.clamped().max_request_bytes, MIN_REQUEST_BYTES);
    assert_eq!(BatchConfig::default().clamped(), BatchConfig::default());
    assert!(run(Vec::new(), BatchConfig::default()).requests.is_empty());
}
