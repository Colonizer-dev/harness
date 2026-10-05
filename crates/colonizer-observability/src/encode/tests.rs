use super::*;
use crate::batch::{BatchConfig, Batcher};
use crate::policy::{ContentGate, Policy, PolicyConfig, Source, SpanKind, Tier};
use crate::proto::logs::v1::SeverityNumber;
use crate::proto::trace::v1::status::StatusCode;
use std::path::PathBuf;

const TRACE_ID: [u8; 16] = [
    0x0a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x01, 0x23, 0x45, 0x67,
];
const SPAN_ID: [u8; 8] = [0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10];
/// Past 2^53, where a JSON number would lose precision: why OTLP/JSON sends 64-bit ints as strings.
const T: u64 = 1_700_000_000_123_456_789;

/// One request of each signal, fixed, exercising ids, times, every attribute value type, a
/// structured body, a status, and both metric point types.
fn requests(encoding: Encoding) -> Vec<Request> {
    let policy = Policy::new(PolicyConfig::default(), ContentGate::closed(), None);
    let resource = policy.resource(&[
        ("service.name", "colonizer".into()),
        ("colonizer.install.id", "install-1".into()),
    ]);
    let mut batcher = Batcher::new(
        &resource,
        BatchConfig {
            encoding,
            ..BatchConfig::default()
        },
    );
    batcher.push(
        policy
            .log(Source::Gateway)
            .time(T)
            .severity(SeverityNumber::Info)
            .trace(TRACE_ID, SPAN_ID)
            .event_name("gateway.request")
            .attr("model", "claude-sonnet", Tier::Structure)
            .attr("input_tokens", 9_007_199_254_740_993_i64, Tier::Structure)
            .attr("duration_ms", 12.5, Tier::Structure)
            .attr("fallback", false, Tier::Structure)
            .body_json(
                &serde_json::json!({ "status": 200, "steps": ["queue", "send"] }),
                Tier::Structure,
            )
            .finish(),
    );
    batcher.push(
        policy
            .span(SpanKind::ExecuteTool, "Bash")
            .ids(TRACE_ID, SPAN_ID, Some([1, 2, 3, 4, 5, 6, 7, 8]))
            .times(T, T + 1_000_000)
            .status(StatusCode::Error)
            .attr("tool.name", "Bash", Tier::Structure)
            .attr("is_error", true, Tier::Structure)
            .finish(),
    );
    batcher.push(
        policy
            .metric(Source::Gateway, "colonizer.gateway.tokens", "{token}")
            .sum(true)
            .times(T - 60_000_000_000, T)
            .attr("model", "claude-sonnet", Tier::Structure)
            .int(9_007_199_254_740_993)
            .finish(),
    );
    batcher.push(
        policy
            .metric(Source::Gateway, "colonizer.gateway.queue", "ms")
            .times(0, T)
            .double(0.25)
            .finish(),
    );
    batcher.finish().requests
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/otlp")
        .join(format!("{name}.json"))
}

/// `value` with the scope's version, which a release bumps, replaced by a fixed stand-in.
fn unversioned(value: &Value) -> Value {
    let version = format!(r#""version":"{}""#, env!("CARGO_PKG_VERSION"));
    serde_json::from_str(&value.to_string().replace(&version, r#""version":"<crate version>""#)).unwrap()
}

#[test]
fn json_matches_the_golden() {
    for request in requests(Encoding::Json) {
        let name = request.path().trim_start_matches("/v1/");
        let value = unversioned(&to_json_value(&request));
        let path = golden(name);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap() + "\n").unwrap();
        }
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}; regenerate with UPDATE_GOLDEN=1", path.display())),
        )
        .unwrap();
        assert_eq!(
            value, expected,
            "{name}: regenerate with UPDATE_GOLDEN=1 if the change is intended"
        );
        // The compact bytes are that value, nothing else.
        assert_eq!(
            unversioned(&serde_json::from_slice::<Value>(&to_json(&request)).unwrap()),
            expected
        );
    }
}

#[test]
fn json_follows_the_otlp_mapping() {
    let all: Vec<String> = requests(Encoding::Json)
        .iter()
        .map(|r| String::from_utf8(to_json(r)).unwrap())
        .collect();
    let all = all.join("\n");
    // Ids are lowercase hex, not base64.
    assert!(all.contains(r#""traceId":"0abcdef0123456789abcdef001234567""#), "{all}");
    assert!(all.contains(r#""spanId":"fedcba9876543210""#));
    assert!(all.contains(r#""parentSpanId":"0102030405060708""#));
    // 64-bit integers are strings: times, attribute ints and integer points alike.
    assert!(all.contains(&format!(r#""timeUnixNano":"{T}""#)));
    assert!(all.contains(&format!(r#""startTimeUnixNano":"{T}""#)));
    assert!(all.contains(r#""intValue":"9007199254740993""#));
    assert!(all.contains(r#""asInt":"9007199254740993""#));
    assert!(all.contains(r#""asDouble":0.25"#));
    // lowerCamelCase field names, enums as integers, no nulls.
    assert!(all.contains(r#""severityNumber":9"#) && all.contains(r#""severityText":"INFO""#));
    assert!(all.contains(r#""aggregationTemporality":2"#) && all.contains(r#""isMonotonic":true"#));
    assert!(all.contains(r#""code":2"#));
    assert!(!all.contains("null") && !all.contains("_unix_nano"));
}

#[test]
fn protobuf_round_trips() {
    for request in requests(Encoding::Protobuf) {
        let bytes = to_protobuf(&request);
        assert_eq!(bytes.len(), request.encoded_len(Encoding::Protobuf));
        let decoded = match &request {
            Request::Logs(_) => Request::Logs(ExportLogsServiceRequest::decode(bytes.as_slice()).unwrap()),
            Request::Traces(_) => Request::Traces(ExportTraceServiceRequest::decode(bytes.as_slice()).unwrap()),
            Request::Metrics(_) => Request::Metrics(ExportMetricsServiceRequest::decode(bytes.as_slice()).unwrap()),
        };
        assert_eq!(decoded, request);
        assert_eq!(request.encode(Encoding::Protobuf), bytes);
    }
}

#[test]
fn gzip_round_trips() {
    use std::io::Read;
    let bytes = to_json(&requests(Encoding::Json)[0]);
    let zipped = gzip(&bytes);
    assert_eq!(zipped[..2], [0x1f, 0x8b]);
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(zipped.as_slice()).read_to_end(&mut out).unwrap();
    assert_eq!(out, bytes);
}
