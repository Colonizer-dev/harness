//! The native Loki push provider (#865): logs only, as Loki JSON to `POST {url}/loki/api/v1/push`,
//! one request per OTLP batch, reusing the transport's retry, cursor and policy machinery.
//!
//! Label keys and values are a fixed low-cardinality set (docs/design/observability.md,
//! "Cardinality and Loki labels"): `service_name`, `stream`, `level`, and `org` — the last only
//! when the record carries `colonizer.org`, which the mapper sets only when the operator gave the
//! colony's org a policy, so it is opt-in by construction. Nothing else ever becomes a label key
//! or value; everything else rides as structured metadata or, with structured metadata off,
//! inside one JSON object per line. Loki rejects out-of-order timestamps within a stream and lines
//! over 256 KiB; the policy's caps bound a line, and sorting each stream's values is all this
//! module does about ordering.

use crate::encode::{Request, json_bytes};
use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::common::v1::{AnyValue, KeyValue, any_value};
use crate::proto::logs::v1::LogRecord;
use crate::proto::resource::v1::Resource;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The push path every base URL is completed with.
pub const PUSH_PATH: &str = "/loki/api/v1/push";
/// The header a Loki tenant rides in.
pub(crate) const TENANT_HEADER: &str = "x-scope-orgid";
/// The service name a request whose resource names none is labelled with.
const DEFAULT_SERVICE: &str = "colonizer";

/// Where a base URL is pushed to: trailing `/`s are dropped, a URL that already ends with the push
/// path is kept as it is (Grafana Cloud hands out full push URLs), and any other base gets the
/// path appended.
pub fn push_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with(PUSH_PATH) {
        base.to_string()
    } else {
        format!("{base}{PUSH_PATH}")
    }
}

/// The tenant header for `tenant_id`, or `None` for an empty one. The caller adds it below the
/// operator's own headers, so an operator-supplied `X-Scope-OrgID` wins over the setting.
pub fn tenant_header(tenant_id: &str) -> Option<(String, String)> {
    (!tenant_id.is_empty()).then(|| (TENANT_HEADER.to_string(), tenant_id.to_string()))
}

/// The Loki JSON push body of one request: `Logs` only, grouped into one stream per label set,
/// each stream's values sorted by timestamp (Loki refuses out-of-order entries within a stream).
/// `Traces` and `Metrics` are an error — the mothership refuses a Loki module with their streams
/// on, and this is the belt behind that brace.
pub fn push_body(request: &Request, structured_metadata: bool) -> Result<Vec<u8>, String> {
    let Request::Logs(request) = request else {
        return Err("the Loki provider takes logs only".into());
    };
    build(request, structured_metadata).map(json_bytes)
}

/// One batch's entries grouped by label set: the grouping that keeps a stream whole, ordered by
/// label set so the same batch always builds the same body.
type Grouped = BTreeMap<Vec<(String, String)>, Vec<(u64, Value)>>;

fn build(request: &ExportLogsServiceRequest, structured_metadata: bool) -> Result<Value, String> {
    let mut streams: Grouped = BTreeMap::new();
    for resource_logs in &request.resource_logs {
        let service = service_name(resource_logs.resource.as_ref());
        for scope in &resource_logs.scope_logs {
            for record in &scope.log_records {
                // A record with no body has no line; there is nothing for Loki to hold.
                let Some(body) = record.body.as_ref() else { continue };
                let mut labels = vec![
                    ("service_name".to_string(), service.to_string()),
                    (
                        "stream".to_string(),
                        str_attr(&record.attributes, "colonizer.stream")
                            .unwrap_or("unknown")
                            .to_string(),
                    ),
                    ("level".to_string(), level(record.severity_number).to_string()),
                ];
                if let Some(org) = str_attr(&record.attributes, "colonizer.org").filter(|org| !org.is_empty()) {
                    labels.push(("org".to_string(), org.to_string()));
                }
                labels.sort();
                let entries = extra_entries(record);
                let ts = Value::String(record.time_unix_nano.to_string());
                let entry = if structured_metadata {
                    let line = Value::String(any_string(Some(body)));
                    let metadata = Map::from_iter(entries.iter().cloned().map(|(k, v)| (k, Value::String(v))));
                    Value::Array(match metadata.is_empty() {
                        true => vec![ts, line],
                        false => vec![ts, line, Value::Object(metadata)],
                    })
                } else {
                    Value::Array(vec![ts, Value::String(line_json(body, &entries).to_string())])
                };
                streams.entry(labels).or_default().push((record.time_unix_nano, entry));
            }
        }
    }
    let streams: Vec<Value> = streams
        .into_iter()
        .map(|(labels, mut values)| {
            values.sort_by_key(|(ts, _)| *ts);
            json!({
                "stream": Map::from_iter(labels.into_iter().map(|(k, v)| (k, Value::String(v)))),
                "values": values.into_iter().map(|(_, entry)| entry).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({"streams": streams}))
}

/// The record's other entries — every attribute as a string, plus `event_name` and the trace and
/// span ids — sorted: Loki structured metadata when it is on, top-level keys of one JSON line when
/// it is off.
fn extra_entries(record: &LogRecord) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = record
        .attributes
        .iter()
        .map(|kv| (kv.key.clone(), any_string(kv.value.as_ref())))
        .collect();
    if !record.event_name.is_empty() {
        out.push(("event_name".to_string(), record.event_name.clone()));
    }
    if !record.trace_id.is_empty() {
        out.push(("trace_id".to_string(), crate::map::hex(&record.trace_id)));
    }
    if !record.span_id.is_empty() {
        out.push(("span_id".to_string(), crate::map::hex(&record.span_id)));
    }
    out.sort();
    out.dedup();
    out
}

/// The line of one record with structured metadata off: the body plus every other entry, as one
/// compact JSON object.
fn line_json(body: &AnyValue, entries: &[(String, String)]) -> Value {
    let mut line = Map::new();
    line.insert("message".to_string(), Value::String(any_string(Some(body))));
    for (key, value) in entries {
        line.insert(key.clone(), Value::String(value.clone()));
    }
    Value::Object(line)
}

/// An OTLP value as the string a line or a metadata entry carries: a string as it is, any other
/// variant as its compact JSON.
fn any_string(value: Option<&AnyValue>) -> String {
    let Some(value) = value else { return "null".into() };
    match value.value.as_ref() {
        Some(any_value::Value::StringValue(s)) => s.clone(),
        other => serde_json::to_string(&other).unwrap_or_else(|_| "null".into()),
    }
}

/// The resource's `service.name`, or the fallback when the resource names none.
fn service_name(resource: Option<&Resource>) -> &str {
    resource
        .and_then(|r| str_attr(&r.attributes, "service.name"))
        .unwrap_or(DEFAULT_SERVICE)
}

/// The first string value of `key`, or `None` when the key is absent or is not a string.
fn str_attr<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| match kv.value.as_ref()?.value.as_ref()? {
            any_value::Value::StringValue(s) => Some(s.as_str()),
            _ => None,
        })
}

/// The Loki `level` label for an OTLP severity number: the bucket's lowercase name, `info` for the
/// unspecified number and the odd values between buckets.
fn level(severity_number: i32) -> &'static str {
    match severity_number {
        1..=4 => "trace",
        5..=8 => "debug",
        9..=12 => "info",
        13..=16 => "warn",
        17..=20 => "error",
        21..=24 => "fatal",
        _ => "info",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
    use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
    use crate::proto::common::v1::{InstrumentationScope, KeyValue};
    use crate::proto::logs::v1::{ResourceLogs, ScopeLogs, SeverityNumber};

    fn str_kv(key: &str, value: &str) -> KeyValue {
        KeyValue {
            key: key.into(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(value.into())),
            }),
            ..KeyValue::default()
        }
    }

    fn record(ts: u64, severity: SeverityNumber, body: &str, attrs: &[(&str, &str)]) -> LogRecord {
        LogRecord {
            time_unix_nano: ts,
            observed_time_unix_nano: ts,
            severity_number: severity as i32,
            body: Some(AnyValue {
                value: Some(any_value::Value::StringValue(body.into())),
            }),
            attributes: attrs.iter().map(|(k, v)| str_kv(k, v)).collect(),
            ..LogRecord::default()
        }
    }

    fn logs(resource: &[(&str, &str)], records: &[LogRecord]) -> Request {
        Request::Logs(ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: resource.iter().map(|(k, v)| str_kv(k, v)).collect(),
                    ..Resource::default()
                }),
                scope_logs: vec![ScopeLogs {
                    scope: Some(InstrumentationScope::default()),
                    log_records: records.to_vec(),
                    ..ScopeLogs::default()
                }],
                ..ResourceLogs::default()
            }],
        })
    }

    fn streams_of(body: &[u8]) -> serde_json::Value {
        serde_json::from_slice(body).expect("a Loki body is JSON")
    }

    #[test]
    fn a_push_url_is_completed_unless_it_already_is_one() {
        assert_eq!(push_url("http://loki:3100"), "http://loki:3100/loki/api/v1/push");
        assert_eq!(push_url("http://loki:3100/"), "http://loki:3100/loki/api/v1/push");
        assert_eq!(push_url("http://loki:3100//"), "http://loki:3100/loki/api/v1/push");
        // Grafana Cloud hands out full push URLs; they pass through, trailing slash or not.
        assert_eq!(
            push_url("https://logs.example.com/loki/api/v1/push"),
            "https://logs.example.com/loki/api/v1/push"
        );
        assert_eq!(
            push_url("https://logs.example.com/loki/api/v1/push/"),
            "https://logs.example.com/loki/api/v1/push"
        );
    }

    #[test]
    fn a_tenant_becomes_a_header_only_when_set() {
        assert_eq!(tenant_header(""), None);
        assert_eq!(tenant_header("acme"), Some(("x-scope-orgid".to_string(), "acme".to_string())));
    }

    #[test]
    fn traces_and_metrics_have_no_loki_body() {
        let traces = Request::Traces(ExportTraceServiceRequest::default());
        let metrics = Request::Metrics(ExportMetricsServiceRequest::default());
        for request in [&traces, &metrics] {
            assert_eq!(push_body(request, true).unwrap_err(), "the Loki provider takes logs only");
        }
    }

    #[test]
    fn records_group_into_sorted_streams_with_metadata_and_sorted_values() {
        let mut first = record(
            2_000_000_000,
            SeverityNumber::Info,
            "line two",
            &[("colonizer.record.id", "r2"), ("colonizer.stream", "operational")],
        );
        first.event_name = "colonizer.harness_log".into();
        first.trace_id = vec![0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 1, 2, 3, 4, 5, 6, 7, 8];
        first.span_id = vec![0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30, 0x31];
        // Same labels as `first`, older: the stream's values come out sorted, not in read order.
        let older = record(
            1_000_000_000,
            SeverityNumber::Info,
            "line one",
            &[("colonizer.record.id", "r1"), ("colonizer.stream", "operational")],
        );
        let org_record = record(
            1_500_000_000,
            SeverityNumber::Warn,
            "line three",
            &[("colonizer.stream", "activity"), ("colonizer.org", "acme")],
        );
        // No body: skipped, not an empty line.
        let bodiless = LogRecord {
            time_unix_nano: 3_000_000_000,
            severity_number: SeverityNumber::Error as i32,
            attributes: vec![str_kv("colonizer.stream", "operational")],
            ..LogRecord::default()
        };
        let request = logs(
            &[("service.name", "colonizer-mothership"), ("host.name", "host-1")],
            &[first, older.clone(), org_record, bodiless],
        );
        let body = push_body(&request, true).unwrap();
        let parsed = streams_of(&body);
        let streams = parsed["streams"].as_array().unwrap();
        assert_eq!(streams.len(), 2, "two label sets, the bodiless record nothing: {parsed}");

        // Sorted label-set order: `activity` sorts after the operational stream's `info`.
        let (a, b) = (&streams[0], &streams[1]);
        assert_eq!(
            a["stream"],
            json!({"level": "info", "service_name": "colonizer-mothership", "stream": "operational"})
        );
        assert_eq!(
            b["stream"],
            json!({"level": "warn", "org": "acme", "service_name": "colonizer-mothership", "stream": "activity"}),
            "the org label is there only for the record that carries the attribute"
        );

        let values = a["values"].as_array().unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0][0], json!("1000000000"), "the older line is first: {values:?}");
        assert_eq!(values[0][1], json!("line one"));
        assert_eq!(values[1][0], json!("2000000000"));
        assert_eq!(values[1][1], json!("line two"));
        // Metadata: every attribute as a string, plus the event name and the ids, keys sorted.
        assert_eq!(
            values[1][2],
            json!({
                "colonizer.record.id": "r2",
                "colonizer.stream": "operational",
                "event_name": "colonizer.harness_log",
                "span_id": "2a2b2c2d2e2f3031",
                "trace_id": "0a0b0c0d0e0f10110102030405060708",
            })
        );
        // A record with no attributes and no ids gets no third element.
        let bare = record(500_000_000, SeverityNumber::Info, "bare", &[]);
        let bare = streams_of(&push_body(&logs(&[("service.name", "colonizer-mothership")], &[bare]), true).unwrap());
        let bare = bare["streams"][0]["values"].as_array().unwrap();
        assert_eq!(bare[0].as_array().unwrap().len(), 2, "{bare:?}");
        // The same request always builds the same body: sorted streams, sorted values, sorted
        // metadata keys, byte for byte.
        assert_eq!(push_body(&request, true).unwrap(), body);
    }

    #[test]
    fn without_structured_metadata_the_line_is_one_json_object() {
        let mut record = record(
            1_000_000_000,
            SeverityNumber::Error,
            "boom",
            &[("colonizer.stream", "operational"), ("attempt", "3")],
        );
        record.event_name = "colonizer.harness_log".into();
        record.trace_id = vec![1_u8; 16];
        record.span_id = vec![2_u8; 8];
        let request = logs(&[("service.name", "colonizer-mothership")], &[record]);
        let parsed = streams_of(&push_body(&request, false).unwrap());
        let values = parsed["streams"][0]["values"].as_array().unwrap();
        assert_eq!(values.len(), 1);
        let entry = values[0].as_array().unwrap();
        assert_eq!(entry.len(), 2, "no metadata element: {entry:?}");
        assert_eq!(entry[0], json!("1000000000"));
        let line: serde_json::Value = serde_json::from_str(entry[1].as_str().unwrap()).unwrap();
        assert_eq!(
            line,
            json!({
                "attempt": "3",
                "colonizer.stream": "operational",
                "event_name": "colonizer.harness_log",
                "message": "boom",
                "span_id": "0202020202020202",
                "trace_id": "01010101010101010101010101010101",
            }),
            "the body and every other entry are top-level keys: {line}"
        );
    }

    /// The acceptance criterion for #865: whatever a record carries — colony ids, repository
    /// names, event names, free text — none of it may become a label key or a label value. Label
    /// values derive only from fixed enums and the operator-set org.
    #[test]
    fn no_record_detail_ever_becomes_a_label() {
        let markers = ["colonizer-colony-abc123", "some-org/repo-name"];
        let records = [
            record(
                1_000_000_000,
                SeverityNumber::Info,
                "colonizer-colony-abc123 hit a snag",
                &[
                    ("colonizer.stream", "activity"),
                    ("colonizer.colony.id", "colonizer-colony-abc123"),
                    ("colonizer.repo", "some-org/repo-name"),
                ],
            ),
            record(
                1_100_000_000,
                SeverityNumber::Fatal,
                "some-org/repo-name failed",
                &[
                    ("colonizer.stream", "operational"),
                    ("colonizer.colony.id", "colonizer-colony-abc123"),
                ],
            ),
            record(1_200_000_000, SeverityNumber::Trace, "free text body", &[]),
            record(1_300_000_000, SeverityNumber::Debug, "another", &[("colonizer.org", "acme")]),
        ];
        let mut named = records[0].clone();
        named.event_name = "colonizer-colony-abc123".into();
        let request = logs(
            &[("service.name", "colonizer-mothership")],
            &[named, records[1].clone(), records[2].clone(), records[3].clone()],
        );
        let parsed = streams_of(&push_body(&request, true).unwrap());
        let streams = parsed["streams"].as_array().unwrap();
        assert!(!streams.is_empty());
        for stream in streams {
            let labels = stream["stream"].as_object().unwrap();
            for (key, value) in labels {
                assert!(
                    ["service_name", "stream", "level", "org"].contains(&key.as_str()),
                    "{key} is not a fixed label key: {labels:?}"
                );
                for marker in markers {
                    assert!(
                        !value.as_str().unwrap().contains(marker),
                        "the marker leaked into a label: {labels:?}"
                    );
                }
            }
            for key in ["stream", "org"] {
                if let Some(value) = labels.get(key) {
                    let value = value.as_str().unwrap();
                    if key == "stream" {
                        assert!(["operational", "activity", "meta", "unknown"].contains(&value), "{value}");
                    }
                }
            }
            assert!(
                ["trace", "debug", "info", "warn", "error", "fatal"].contains(&labels["level"].as_str().unwrap()),
                "{}",
                labels["level"]
            );
        }
        // Not vacuous: the markers do ride as values and metadata, where Loki keeps them unindexed.
        let body = String::from_utf8(push_body(&request, true).unwrap()).unwrap();
        for marker in markers {
            assert!(body.contains(marker), "{marker} should still be in the body: {body}");
        }
    }

    #[test]
    fn a_resource_without_a_service_name_falls_back() {
        let request = logs(&[], &[record(1, SeverityNumber::Info, "x", &[])]);
        let parsed = streams_of(&push_body(&request, true).unwrap());
        assert_eq!(parsed["streams"][0]["stream"]["service_name"], json!("colonizer"));
        // A severity number that is 0 or between buckets is `info`, and a missing stream is
        // `unknown` — both fixed values, never absent labels.
        let request = logs(&[("service.name", "s")], &[record(1, SeverityNumber::Unspecified, "x", &[])]);
        let parsed = streams_of(&push_body(&request, true).unwrap());
        assert_eq!(parsed["streams"][0]["stream"]["level"], json!("info"));
        assert_eq!(parsed["streams"][0]["stream"]["stream"], json!("unknown"));
    }
}
