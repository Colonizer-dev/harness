//! The canary kit's own tests: every canary is caught by the redactor, and `assert_absent` catches
//! a leak in every form it claims to.

use super::*;
use colonizer_redact::redact_text;

#[test]
fn redaction_catches_every_canary_in_context() {
    let canaries = Canaries::new();
    assert_eq!(canaries.all.len(), 15);
    for canary in &canaries.all {
        let redacted = redact_text(&canary.text);
        for secret in &canary.secrets {
            assert!(!redacted.contains(secret.as_str()), "{}: {redacted}", canary.name);
        }
        for kind in &canary.marks {
            assert!(
                redacted.contains(&format!("[REDACTED:{kind}]")),
                "{}: {redacted}",
                canary.name
            );
        }
        assert!(redacted.contains("[REDACTED:"), "{}: {redacted}", canary.name);
    }
}

#[test]
fn canaries_are_fresh_each_run() {
    let (a, b) = (Canaries::new(), Canaries::new());
    assert!(a.secrets().zip(b.secrets()).all(|(x, y)| x != y));
    assert_ne!(a.content, b.content);
}

#[test]
fn assert_absent_finds_raw_base64_percent_gzipped_and_protobuf_leaks() {
    use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
    use crate::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
    use prost::Message;
    let canaries = Canaries::new();
    let secret = canaries.all[10].secrets[0].as_str();
    let probe = format!("{secret}:@/");
    let json = |s: String| serde_json::to_vec(&serde_json::json!({ "v": s })).unwrap();
    let protobuf = ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord {
                    event_name: format!("e {secret}"),
                    ..LogRecord::default()
                }],
                ..ScopeLogs::default()
            }],
            ..ResourceLogs::default()
        }],
    }
    .encode_to_vec();
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (json(format!("x {secret} y")), secret),
        (json(STANDARD.encode(format!("pre{secret}post"))), secret),
        (json(URL_SAFE.encode(format!("pr{secret}"))), secret),
        (json(needles(&probe)[1].clone()), &probe),
        (crate::encode::gzip(&json(secret.to_string())), secret),
        (protobuf, secret),
    ];
    for (i, (leak, secret)) in cases.iter().enumerate() {
        let caught = std::panic::catch_unwind(|| assert_absent(leak, [*secret], "probe"));
        assert!(caught.is_err(), "leak {i} not caught");
    }
    assert_absent(&json("nothing here".into()), canaries.secrets(), "clean");
}
