//! The export policy's acceptance tests (#844): canaries never leave in any encoding, content stays
//! behind a closed gate, caps never split a character, hashed names never leave in plain, unknown
//! keys are a bug, and payloads become placeholders.

use super::*;
use crate::batch::{BatchConfig, Batcher};
use crate::encode::{Encoding, Request, gzip, to_json, to_protobuf};
use crate::hashing::{HashKey, key_path, temp_dir};
use crate::proto::trace::v1::status::StatusCode;
use crate::testkit::{Canaries, assert_absent, strings};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;

fn policy(gate: ContentGate, config: PolicyConfig) -> Policy {
    Policy::new(config, gate, None)
}

/// Every request of `items`, packed in both encodings, as protobuf, JSON and gzip of each.
fn exported(policy: &Policy, items: Vec<Item>) -> Vec<(String, Vec<u8>)> {
    let resource = policy.resource(&[("service.name", "colonizer".into())]);
    let mut out = Vec::new();
    for encoding in [Encoding::Protobuf, Encoding::Json] {
        let mut batcher = Batcher::new(
            &resource,
            BatchConfig {
                encoding,
                ..BatchConfig::default()
            },
        );
        items.iter().cloned().for_each(|i| batcher.push(i));
        let batch = batcher.finish();
        assert_eq!(batch.oversized, 0);
        for (n, request) in batch.requests.iter().enumerate() {
            let (pb, js) = (to_protobuf(request), to_json(request));
            out.push((format!("{encoding:?} #{n} {} protobuf", request.path()), pb.clone()));
            out.push((format!("{encoding:?} #{n} {} json", request.path()), js.clone()));
            out.push((format!("{encoding:?} #{n} {} protobuf+gzip", request.path()), gzip(&pb)));
            out.push((format!("{encoding:?} #{n} {} json+gzip", request.path()), gzip(&js)));
        }
    }
    out
}

fn log_of(item: &Item) -> &LogRecord {
    match &item.0 {
        Record::Log(r) => r,
        other => panic!("not a log: {other:?}"),
    }
}

fn span_of(item: &Item) -> &Span {
    match &item.0 {
        Record::Span(s) => s,
        other => panic!("not a span: {other:?}"),
    }
}

fn attr<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a any_value::Value> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref()?.value.as_ref())
}

fn str_attr<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a str> {
    match attr(attributes, key)? {
        any_value::Value::StringValue(s) => Some(s),
        _ => None,
    }
}

fn body_text(record: &LogRecord) -> &str {
    match record.body.as_ref().and_then(|b| b.value.as_ref()) {
        Some(any_value::Value::StringValue(s)) => s,
        other => panic!("not a text body: {other:?}"),
    }
}

/// Whether colonizer-redact itself keeps `text` as the field `key` of a JSON line: an identifier key
/// exempts its value from the entropy layer, on disk and here alike.
fn kept_on_disk(key: &str, text: &str) -> bool {
    let mut field = json!({ key: text });
    !colonizer_redact::redact_value(&mut field)
}

/// `text` at every string position a log record, span, metric point and resource has: event name,
/// text and JSON body (as a value and as a key), every allowlisted attribute of every source and
/// span kind, a span's name, a resource attribute. An attribute key under which the redactor keeps
/// `text` on disk too ([`kept_on_disk`]) is left out, and returned.
fn everywhere(policy: &Policy, text: &str) -> (Vec<Item>, Vec<String>) {
    let mut items = Vec::new();
    let mut kept = Vec::new();
    let mut keys_for = |tables: &[&'static [allowlist::Rule]; 2]| -> Vec<String> {
        let (out, exempt): (Vec<_>, Vec<_>) = allowlist::every_key(tables)
            .into_iter()
            .map(|(k, _, _)| k)
            .partition(|k| !kept_on_disk(k, text));
        kept.extend(exempt);
        out
    };
    for source in Source::ALL {
        let keys = keys_for(&allowlist::for_source(source));
        let mut log = policy.log(source).event_name(text).body(text, Tier::Content);
        let mut metric = policy.metric(source, "colonizer.test", "1");
        for key in &keys {
            log = log.attr(key, text, Tier::Structure);
            metric = metric.attr(key, text, Tier::Structure);
        }
        items.push(log.finish());
        items.push(metric.int(1).finish());
        let mut body = json!({ "text": text, "nested": [{ "deeper": text }] });
        body[text] = json!("as a key");
        items.push(policy.log(source).body_json(&body, Tier::Content).finish());
    }
    for kind in SpanKind::ALL {
        let mut span = policy.span(kind, text).ids([1; 16], [2; 8], None).status(StatusCode::Error);
        for key in keys_for(&allowlist::for_span(kind)) {
            span = span.attr(&key, text, Tier::Structure);
        }
        items.push(span.finish());
    }
    kept.sort_unstable();
    kept.dedup();
    (items, kept)
}

#[test]
fn no_canary_survives_any_string_position_or_encoding() {
    let canaries = Canaries::new();
    // The gate open, so content positions are filled too: redaction must hold there as well.
    let policy = policy(ContentGate::open(), PolicyConfig::default());
    for canary in &canaries.all {
        let (items, kept) = everywhere(&policy, &canary.text);
        // Only a bare high-entropy string survives anywhere on disk, and only under an identifier
        // key (`colonizer.record.id`, `tool_call_id`): the entropy layer's one exemption.
        if canary.name == "entropy" {
            assert!(!kept.is_empty());
            for key in &kept {
                let compact: String = key.chars().filter(char::is_ascii_alphanumeric).collect();
                assert!(compact.to_ascii_lowercase().ends_with("id"), "{key} keeps an entropy canary");
            }
        } else {
            assert_eq!(kept, Vec::<String>::new(), "{}", canary.name);
        }
        let mut payloads = exported(&policy, items);
        // And as a resource attribute.
        let resource = policy.resource(&[("host.name", AttrValue::Str(canary.text.clone()))]);
        let mut batcher = Batcher::new(&resource, BatchConfig::default());
        batcher.push(policy.log(Source::Harness).finish());
        let request = batcher.finish().requests.remove(0);
        payloads.push(("resource protobuf".to_string(), to_protobuf(&request)));
        payloads.push(("resource json".to_string(), to_json(&request)));
        for (what, bytes) in &payloads {
            assert_absent(
                bytes,
                canary.secrets.iter().map(String::as_str),
                &format!("{}: {what}", canary.name),
            );
            let all = strings(bytes).join("\n");
            assert!(all.contains("[REDACTED:"), "{}: {what} has no redaction mark", canary.name);
            for kind in &canary.marks {
                assert!(
                    all.contains(&format!("[REDACTED:{kind}]")),
                    "{}: {what} lacks {kind}",
                    canary.name
                );
            }
        }
    }
}

#[test]
fn content_never_leaves_through_a_closed_gate_nor_onto_a_span() {
    let canaries = Canaries::new();
    let content = canaries.content.as_str();
    let closed = policy(ContentGate::closed(), PolicyConfig::default());
    let items = vec![
        closed.log(Source::Events).body(content, Tier::Content).finish(),
        closed
            .log(Source::Events)
            .body_json(&json!({ "prompt": content }), Tier::Content)
            .finish(),
        // Content by the allowlist's say-so, whatever tier the caller claims.
        closed.log(Source::Activity).attr("detail", content, Tier::Structure).finish(),
        closed
            .log(Source::Findings)
            .attr("title", content, Tier::Structure)
            .attr("reason", content, Tier::Structure)
            .finish(),
        closed.log(Source::Events).attr("path", content, Tier::Structure).finish(),
        // Content by the caller's say-so, on a structure key.
        closed.log(Source::Events).attr("name", content, Tier::Content).finish(),
        closed
            .span(SpanKind::ExecuteTool, "Bash")
            .attr("tool.name", content, Tier::Content)
            .finish(),
        closed
            .metric(Source::Gateway, "colonizer.test", "1")
            .attr("model", content, Tier::Content)
            .int(1)
            .finish(),
    ];
    for (what, bytes) in exported(&closed, items.clone()) {
        assert_absent(&bytes, [content], &what);
    }
    assert_eq!(log_of(&items[0]).body, None);
    assert!(log_of(&items[2]).attributes.is_empty());

    // Open, the same log values go — but a span still carries none (P5), nor does a metric point.
    let open = policy(ContentGate::open(), PolicyConfig::default());
    let log = open
        .log(Source::Activity)
        .attr("detail", content, Tier::Structure)
        .body(content, Tier::Content)
        .finish();
    assert_eq!(str_attr(&log_of(&log).attributes, "detail"), Some(content));
    assert_eq!(body_text(log_of(&log)), content);
    let spans = vec![
        open.span(SpanKind::Subagent, "review")
            .attr("colonizer.subagent.description", content, Tier::Structure)
            .finish(),
        open.span(SpanKind::ExecuteTool, "Bash")
            .attr("tool.name", content, Tier::Content)
            .finish(),
        open.metric(Source::Activity, "colonizer.test", "1")
            .attr("detail", content, Tier::Structure)
            .int(1)
            .finish(),
    ];
    assert!(span_of(&spans[0]).attributes.is_empty());
    for (what, bytes) in exported(&open, spans) {
        assert_absent(&bytes, [content], &what);
    }
}

/// Checks `out` is `input` cut at a character boundary and marked, within `cap`.
fn assert_cut(input: &str, out: &str, cap: usize) {
    assert!(out.len() <= cap, "{} > {cap}", out.len());
    let (kept, marker) = out.split_once('…').unwrap_or_else(|| panic!("no marker in {out:?}"));
    assert!(input.starts_with(kept), "the kept part is a prefix");
    assert!(input.is_char_boundary(kept.len()));
    assert_eq!(marker, format!("({} more bytes)", input.len() - kept.len()));
}

#[test]
fn truncation_never_splits_a_character_and_says_what_it_cut() {
    // 4-byte clef, a ZWJ emoji sequence (4 + 3 + 4 bytes), 2-byte and 1-byte characters.
    let unit = "a𝄞é👩\u{200d}💻";
    let long = unit.repeat(2000);
    for cap in 32..=140 {
        let out = truncate(&long, cap);
        assert_cut(&long, &out, cap);
    }
    assert!(matches!(truncate("short", 5), std::borrow::Cow::Borrowed("short")));
    // A cap too small for any marker still cuts on a boundary.
    assert_eq!(truncate("𝄞𝄞𝄞", 5), "𝄞");

    for cap in [128, 129, 130, 131, 1000] {
        let p = policy(
            ContentGate::open(),
            PolicyConfig {
                max_attribute_bytes: cap,
                max_content_bytes: 1024 + cap,
                ..PolicyConfig::default()
            },
        );
        let item = p
            .log(Source::Events)
            .attr("name", long.as_str(), Tier::Structure)
            .body(&long, Tier::Content)
            .finish();
        let record = log_of(&item);
        assert_cut(&long, str_attr(&record.attributes, "name").unwrap(), cap);
        assert_cut(&long, body_text(record), p.config().max_content_bytes);
        assert_eq!(attr(&record.attributes, TRUNCATED), Some(&any_value::Value::BoolValue(true)));
        let span = p.span(SpanKind::ExecuteTool, &long).finish();
        assert_cut(&format!("execute_tool {long}"), &span_of(&span).name, cap);
        assert_eq!(
            attr(&span_of(&span).attributes, TRUNCATED),
            Some(&any_value::Value::BoolValue(true))
        );
    }
    // Nothing cut, no flag.
    let p = policy(ContentGate::open(), PolicyConfig::default());
    let item = p.log(Source::Events).attr("name", "Bash", Tier::Structure).finish();
    assert_eq!(attr(&log_of(&item).attributes, TRUNCATED), None);
}

#[test]
fn hashed_names_never_leave_in_plain() {
    let dir = temp_dir("policy-hashed");
    let key = HashKey::load_or_create(&dir).unwrap();
    let path = key_path(&dir);
    let key_bytes = std::fs::read(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let config = PolicyConfig {
        repo_names: RepoNames::Hashed,
        ..PolicyConfig::default()
    };
    // The gate open: `hashed` drops titles even when content is allowed.
    let p = Policy::new(config, ContentGate::open(), Some(key.clone()));
    let (org, repo, branch, title) = (
        "acme-corp",
        "acme-corp/widget-factory",
        "feature/quiet-launch",
        "Rework the billing flow",
    );
    let pr = "https://github.com/acme-corp/widget-factory/pull/7";
    let items = vec![
        p.log(Source::Activity)
            .attr("colonizer.org", org, Tier::Structure)
            .attr("colonizer.repo", repo, Tier::Structure)
            .attr("colonizer.branch", branch, Tier::Structure)
            .attr("colonizer.pr.url", pr, Tier::Structure)
            .attr("colonizer.issue.title", title, Tier::Content)
            .attr("colonizer.pr.title", title, Tier::Content)
            .attr("repo", repo, Tier::Structure)
            .finish(),
        p.log(Source::Findings)
            .attr("issue", format!("https://github.com/{repo}/issues/3"), Tier::Structure)
            .attr("duplicate_of", format!("https://github.com/{repo}/issues/1"), Tier::Structure)
            .attr("pr", pr, Tier::Structure)
            .attr("title", title, Tier::Structure)
            .finish(),
        p.log(Source::Spend).attr("org", org, Tier::Structure).finish(),
        p.span(SpanKind::InvokeAgent, repo)
            .attr("colonizer.repo", repo, Tier::Structure)
            .finish(),
        p.metric(Source::Spend, "colonizer.cost", "USD")
            .attr("org", org, Tier::Structure)
            .double(0.5)
            .finish(),
        // A `workspace.*` activity entry's target is its org.
        p.log(Source::Activity)
            .attr("kind", "workspace.settings", Tier::Structure)
            .attr("target", org, Tier::Structure)
            .finish(),
    ];
    let hashed = key.hash_name(repo);
    assert_eq!(
        str_attr(&log_of(&items[0]).attributes, "colonizer.repo"),
        Some(hashed.as_str())
    );
    assert_eq!(
        str_attr(&log_of(&items[0]).attributes, "colonizer.org"),
        Some(key.hash_name(org).as_str())
    );
    assert_eq!(span_of(&items[3]).name, format!("invoke_agent {hashed}"));
    assert_eq!(
        str_attr(&log_of(&items[5]).attributes, "target"),
        Some(key.hash_name(org).as_str())
    );
    let mut saw_hash = false;
    for (what, bytes) in exported(&p, items) {
        assert_absent(&bytes, ["acme-corp", "widget-factory", "quiet-launch", title], &what);
        saw_hash |= strings(&bytes).iter().any(|s| s.contains(&hashed));
    }
    assert!(saw_hash);

    // A restart reuses the key file as it was; without a key, hashable values are dropped, not sent.
    let again = HashKey::load_or_create(&dir).unwrap();
    assert_eq!(again.hash_name(repo), hashed);
    assert_eq!(std::fs::read(&path).unwrap(), key_bytes);
    let keyless = Policy::new(config, ContentGate::closed(), None);
    let item = keyless
        .log(Source::Activity)
        .attr("colonizer.repo", repo, Tier::Structure)
        .finish();
    assert!(log_of(&item).attributes.is_empty());
    assert_eq!(
        keyless.span(SpanKind::InvokeAgent, repo).finish(),
        keyless.span(SpanKind::InvokeAgent, "").finish()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "not on this record's allowlist")]
fn an_unknown_attribute_key_is_a_bug() {
    let p = policy(ContentGate::closed(), PolicyConfig::default());
    let _ = p.log(Source::Gateway).attr("request_body", "{}", Tier::Structure);
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "not on this record's allowlist")]
fn a_prefixed_key_with_free_text_after_the_prefix_is_unknown() {
    let p = policy(ContentGate::closed(), PolicyConfig::default());
    let _ = p.log(Source::Events).attr("model_usage.input tokens: 5", 5, Tier::Structure);
}

#[test]
#[cfg(not(debug_assertions))]
fn an_unknown_attribute_key_is_dropped_in_release() {
    let p = policy(ContentGate::closed(), PolicyConfig::default());
    let item = p.log(Source::Gateway).attr("request_body", "{}", Tier::Structure).finish();
    assert!(log_of(&item).attributes.is_empty());
}

#[test]
fn allowlists_match_exact_and_prefixed_keys() {
    let events = allowlist::for_source(Source::Events);
    assert!(allowlist::lookup(&events, "model_usage.cache_read_input_tokens").is_some());
    assert!(allowlist::lookup(&events, "model_usage.").is_none());
    assert!(allowlist::lookup(&events, "model_usage").is_none());
    assert!(allowlist::lookup(&events, "colonizer.truncated").is_some());
    assert!(allowlist::lookup(&allowlist::for_source(Source::Gateway), "type").is_none());
    for kind in SpanKind::ALL {
        let tables = allowlist::for_span(kind);
        assert!(allowlist::lookup(&tables, TRUNCATED).is_some(), "{kind:?}");
        // No content key the GenAI conventions define is allowed on a span.
        for key in [
            "gen_ai.input.messages",
            "gen_ai.output.messages",
            "gen_ai.tool.call.arguments",
        ] {
            assert!(allowlist::lookup(&tables, key).is_none(), "{key} on {kind:?}");
        }
    }
    for source in Source::ALL {
        let keys = allowlist::every_key(&allowlist::for_source(source));
        let mut names: Vec<_> = keys.iter().map(|(k, _, _)| k.as_str()).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total, "{source:?} lists a key twice");
    }
}

/// A PNG's base64: its magic bytes, then `n` random-looking bytes.
fn png_base64(n: usize) -> String {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend((0..n).map(|i| (i * 7919 % 251) as u8 ^ (i >> 3) as u8));
    STANDARD.encode(bytes)
}

#[test]
fn images_and_base64_payloads_become_placeholders() {
    let p = policy(ContentGate::open(), PolicyConfig::default());
    let data = png_base64(3000);
    let probe = &data[200..300];
    let block = json!({
        "role": "user",
        "content": [
            { "type": "text", "text": "what is in this picture?" },
            { "type": "image", "source": { "type": "base64", "media_type": "image/jpeg", "data": data } }
        ]
    });
    let items = vec![
        p.log(Source::Events).body_json(&block, Tier::Content).finish(),
        p.log(Source::Events)
            .body(&format!("see data:image/png;base64,{data} above"), Tier::Content)
            .finish(),
        p.log(Source::Events)
            .body(&format!("raw {data} pasted"), Tier::Content)
            .finish(),
        p.log(Source::Events)
            .attr("name", format!("tool {data}"), Tier::Structure)
            .finish(),
    ];
    let decoded = 3008;
    let json_body = serde_json::to_string(&log_of(&items[0]).body).unwrap();
    assert!(
        json_body.contains(&format!("[image image/jpeg {decoded} bytes]")),
        "{json_body}"
    );
    assert!(json_body.contains("what is in this picture?"));
    assert_eq!(
        body_text(log_of(&items[1])),
        format!("see [image image/png {decoded} bytes] above")
    );
    assert_eq!(
        body_text(log_of(&items[2])),
        // Undeclared, a payload is a blob of unknown type: its size is what P8 asks for.
        format!("raw [blob application/octet-stream {decoded} bytes] pasted")
    );
    let name = str_attr(&log_of(&items[3]).attributes, "name").unwrap();
    assert_eq!(name, format!("tool [blob application/octet-stream {decoded} bytes]"));
    for (what, bytes) in exported(&p, items) {
        assert_absent(&bytes, [probe], &what);
    }
    assert_eq!(
        placeholder(Some("Application/PDF"), &STANDARD.encode(b"%PDF-1.7 and more")),
        "[blob application/pdf 17 bytes]"
    );
    assert_eq!(placeholder(None, "AAAAAAAA"), "[blob application/octet-stream 6 bytes]");
    // Text that merely looks long is not a payload: a path, words, a hex digest.
    let path = format!("src/{}/main.rs", "module_name/".repeat(40));
    assert_eq!(scrub::blobs(&path), path.as_str());
    let hex = "0123456789abcdef".repeat(20);
    assert_eq!(scrub::blobs(&hex), hex.as_str());
}

#[test]
fn config_caps_are_held_to_the_settings_ranges() {
    let wild = PolicyConfig {
        max_attribute_bytes: 1,
        max_content_bytes: usize::MAX,
        repo_names: RepoNames::Plain,
    };
    let p = policy(ContentGate::closed(), wild);
    assert_eq!(p.config().max_attribute_bytes, 128);
    assert_eq!(p.config().max_content_bytes, 196_608);
    assert_eq!(PolicyConfig::default().clamped(), PolicyConfig::default());
    assert!(!ContentGate::default().allows());
    assert!(!ContentGate::closed().allows());
}

#[test]
fn records_carry_their_structure() {
    let p = policy(ContentGate::closed(), PolicyConfig::default());
    let log = p
        .log(Source::Gateway)
        .time(5)
        .severity(SeverityNumber::Warn2)
        .trace([3; 16], [4; 8])
        .attr("status", 529, Tier::Structure)
        .attr("fallback", true, Tier::Structure)
        .attr("duration_ms", 12.5, Tier::Structure)
        .attr("status", 503, Tier::Structure)
        .finish();
    let record = log_of(&log);
    assert_eq!((record.time_unix_nano, record.observed_time_unix_nano), (5, 5));
    assert_eq!(record.severity_text, "WARN");
    assert_eq!(attr(&record.attributes, "status"), Some(&any_value::Value::IntValue(503)));
    assert_eq!(record.attributes.len(), 3, "a repeated key replaces the earlier value");
    let span = p
        .span(SpanKind::Chat, "")
        .ids([1; 16], [2; 8], Some([3; 8]))
        .times(1, 2)
        .status(StatusCode::Error)
        .finish();
    let span = span_of(&span);
    assert_eq!(span.name, "chat");
    assert_eq!(span.status.as_ref().map(|s| (s.code, s.message.as_str())), Some((2, "")));
    let request = |item: Item| {
        let mut b = Batcher::new(&p.resource(&[]), BatchConfig::default());
        b.push(item);
        b.finish().requests.remove(0)
    };
    let metric = request(
        p.metric(Source::Spend, "colonizer.cost", "USD")
            .sum(true)
            .double(f64::NAN)
            .finish(),
    );
    assert!(matches!(metric, Request::Metrics(_)));
    assert!(!String::from_utf8(to_json(&metric)).unwrap().contains("null"));
}

#[test]
fn an_identifier_keeps_its_entropy_exemption_but_not_a_known_token() {
    let canaries = Canaries::new();
    let github = &canaries.all.iter().find(|c| c.name == "github").unwrap().secrets[0];
    // A Vertex-routed tool call id: long and random-looking, but an id, and the join key between a
    // tool's request and its result.
    let id = format!("{}_{}_{}", "toolu", "vrtx", "01AbCdEfGhJkMnPqRsTuVwXyZ2345678");
    assert_eq!(id.len(), 43);
    let p = policy(ContentGate::closed(), PolicyConfig::default());
    let kept = |item: &Item, key: &str| {
        let attributes = match &item.0 {
            Record::Log(r) => &r.attributes,
            Record::Span(s) => &s.attributes,
            Record::Metric(_) => unreachable!(),
        };
        str_attr(attributes, key).map(str::to_string)
    };
    for (item, key) in [
        (
            p.log(Source::Events)
                .attr("tool_call_id", id.as_str(), Tier::Structure)
                .finish(),
            "tool_call_id",
        ),
        (
            p.log(Source::JevLadder)
                .attr("matched_tool_call_id", id.as_str(), Tier::Structure)
                .finish(),
            "matched_tool_call_id",
        ),
        (
            p.span(SpanKind::ExecuteTool, "Bash")
                .attr("gen_ai.tool.call.id", id.as_str(), Tier::Structure)
                .finish(),
            "gen_ai.tool.call.id",
        ),
    ] {
        assert_eq!(kept(&item, key).as_deref(), Some(id.as_str()), "{key}");
    }
    // Under a key that is not an identifier, the same value is redacted: the exemption is what keeps it.
    let named = p.log(Source::Events).attr("name", id.as_str(), Tier::Structure).finish();
    assert_eq!(kept(&named, "name").as_deref(), Some("[REDACTED:high_entropy]"));
    // A provider token is still caught under an identifier key.
    let leaked = p
        .log(Source::Events)
        .attr("tool_call_id", github.as_str(), Tier::Structure)
        .finish();
    assert_eq!(kept(&leaked, "tool_call_id").as_deref(), Some("[REDACTED:github_token]"));
    for (what, bytes) in exported(&p, vec![leaked]) {
        assert_absent(&bytes, [github.as_str()], &what);
    }
}

#[test]
fn a_secret_in_a_prefixed_key_drops_the_attribute() {
    let canaries = Canaries::new();
    let p = policy(ContentGate::closed(), PolicyConfig::default());
    for name in ["github", "aws", "stripe", "slack"] {
        let secret = &canaries.all.iter().find(|c| c.name == name).unwrap().secrets[0];
        let item = p
            .log(Source::Events)
            .attr(&format!("model_usage.{secret}"), 5, Tier::Structure)
            .attr("model_usage.claude-sonnet-4", 7, Tier::Structure)
            .finish();
        let keys: Vec<_> = log_of(&item).attributes.iter().map(|kv| kv.key.as_str()).collect();
        assert_eq!(keys, ["model_usage.claude-sonnet-4"], "{name}");
        for (what, bytes) in exported(&p, vec![item]) {
            assert_absent(&bytes, [secret.as_str()], &what);
        }
    }
}
