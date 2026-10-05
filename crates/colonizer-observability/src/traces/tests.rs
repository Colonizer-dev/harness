//! The colony trace: goldens of the span tree for fixed ledgers, the subagent tree, restart safety,
//! turn deltas, the root's single emission, sampling, the open-span cap, and the content canary.
//!
//! Regenerate the goldens with `UPDATE_GOLDEN=1 cargo test -p colonizer-observability traces::`.

use super::*;
use crate::batch::{BatchConfig, Batcher, Record};
use crate::encode::{Encoding, Request, to_json_value};
use crate::policy::{ContentGate, PolicyConfig};
use crate::proto::trace::v1::Span;
use crate::testkit::Canaries;
use std::path::PathBuf;

pub(crate) const HOST: &str = "host-1";
/// A tick's `now`: 2027-01-15, months after every fixture line.
pub(crate) const NOW: u64 = 1_800_000_000_000_000_000;

pub(crate) fn policy() -> Policy {
    Policy::new(PolicyConfig::default(), ContentGate::closed(), None)
}

fn colony_policy(created_at: &str, pr_url: Option<&str>) -> ColonyPolicy {
    ColonyPolicy {
        org: "acme".into(),
        repo: "acme/widgets".into(),
        status: Some("running".into()),
        agent: "claude_code".into(),
        created_at: Some(created_at.into()),
        origin: None,
        pr_url: pr_url.map(str::to_string),
        ..ColonyPolicy::default()
    }
}

pub(crate) fn colonies() -> BTreeMap<String, ColonyPolicy> {
    BTreeMap::from([
        (
            "c1a0dec0".to_string(),
            colony_policy("2026-09-24T09:30:00Z", Some("https://github.com/acme/widgets/pull/7")),
        ),
        ("5ab0a9e7".to_string(), colony_policy("2026-09-24T09:59:00Z", None)),
        ("9e57c0de".to_string(), colony_policy("2026-09-24T09:59:30Z", None)),
        ("a1b2c3d4".to_string(), colony_policy("2026-09-24T09:30:00Z", None)),
        ("6a7e3a11".to_string(), colony_policy("2026-09-24T10:59:00Z", None)),
        (
            "e5f60718".to_string(),
            ColonyPolicy {
                status: Some("stopped".into()),
                origin: Some("burn_down".into()),
                ..colony_policy("2026-09-24T09:30:00Z", None)
            },
        ),
    ])
}

pub(crate) fn fixture(name: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/traces")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

pub(crate) fn outcome(colony: &str, kind: &str, ts: &str) -> Value {
    serde_json::json!({"seq": 1, "ts": ts, "kind": format!("outcome.{kind}"), "actor": "colony", "colony": colony})
}

/// The builder over fixed colonies and a policy.
pub(crate) struct Run<'a> {
    pub colonies: &'a BTreeMap<String, ColonyPolicy>,
    pub policy: Policy,
    pub ratio: f64,
    pub max_trace_bytes: u64,
}

/// The SHA-256 of a line as the tailer reads it: the key of a line with no `seq`.
pub(crate) fn digest(line: &Value) -> [u8; 32] {
    let d = ring::digest::digest(&ring::digest::SHA256, line.to_string().as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

impl Run<'_> {
    pub(crate) fn builder(&self) -> Builder<'_> {
        Builder {
            policy: &self.policy,
            host_id: HOST,
            colonies: self.colonies,
            sample_ratio: self.ratio,
            max_trace_bytes: self.max_trace_bytes,
        }
    }

    /// Feeds `events` of `colony`, then `activity`, then settles the tick.
    pub(crate) fn feed(&self, state: &mut Traces, colony: &str, events: &[Value], activity: &[Value], now: u64) -> Vec<Item> {
        self.feed_all(state, colony, events, &[], activity, now)
    }

    /// Feeds `events` of `colony`, its `gateway` lines, then `activity`, then settles the tick.
    pub(crate) fn feed_all(
        &self,
        state: &mut Traces,
        colony: &str,
        events: &[Value],
        gateway: &[Value],
        activity: &[Value],
        now: u64,
    ) -> Vec<Item> {
        let builder = self.builder();
        let mut out = Vec::new();
        for line in gateway {
            builder.feed(state, Source::Gateway, Some(colony), line, &digest(line), &mut out);
        }
        for line in events {
            builder.feed(state, Source::Events, Some(colony), line, &digest(line), &mut out);
        }
        for line in activity {
            builder.feed(state, Source::Activity, None, line, &digest(line), &mut out);
        }
        builder.settle(state, &|_| true, &[], now, &mut out);
        out
    }
}

pub(crate) fn run(colonies: &BTreeMap<String, ColonyPolicy>) -> Run<'_> {
    Run {
        colonies,
        policy: policy(),
        ratio: 1.0,
        max_trace_bytes: DEFAULT_MAX_TRACE_BYTES,
    }
}

pub(crate) fn spans(items: &[Item]) -> Vec<Span> {
    items
        .iter()
        .map(|i| match &i.0 {
            Record::Span(s) => s.clone(),
            other => panic!("not a span: {other:?}"),
        })
        .collect()
}

pub(crate) fn attr(span: &Span, key: &str) -> Option<Value> {
    let kv = span.attributes.iter().find(|kv| kv.key == key)?;
    let v = serde_json::to_value(kv.value.as_ref()?).ok()?;
    let (kind, v) = v.as_object()?.iter().next()?;
    // OTLP/JSON carries a 64-bit integer as a string.
    match (kind.as_str(), v) {
        ("intValue", Value::String(n)) => n.parse::<i64>().ok().map(Value::from),
        _ => Some(v.clone()),
    }
}

pub(crate) fn by_name<'s>(spans: &'s [Span], name: &str) -> &'s Span {
    spans
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no span {name:?} in {:?}", spans.iter().map(|s| &s.name).collect::<Vec<_>>()))
}

pub(crate) fn request(items: Vec<Item>) -> Request {
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

pub(crate) fn check_golden(name: &str, value: Value) {
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

/// The Claude Code runner's fixture as one merged colony.
fn simple() -> Vec<Item> {
    let colonies = colonies();
    let r = run(&colonies);
    r.feed(
        &mut Traces::default(),
        "c1a0dec0",
        &fixture("claude-code-events.jsonl"),
        &[outcome("c1a0dec0", "merged", "2026-09-24T09:32:00Z")],
        NOW,
    )
}

fn subagents() -> Vec<Item> {
    let colonies = colonies();
    let r = run(&colonies);
    r.feed(
        &mut Traces::default(),
        "5ab0a9e7",
        &fixture("subagents-events.jsonl"),
        &[outcome("5ab0a9e7", "merged", "2026-09-24T10:01:00Z")],
        NOW,
    )
}

#[test]
fn a_simple_colony_maps_to_the_golden_trace() {
    let items = simple();
    let spans = spans(&items);
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "question",
            "execute_tool Bash",
            "execute_tool Bash",
            "host_step boundary",
            "host_step path_policy",
            "execute_tool Read",
            "turn 1",
            "invoke_agent acme/widgets"
        ]
    );
    let root = by_name(&spans, "invoke_agent acme/widgets");
    assert!(root.parent_span_id.is_empty());
    assert_eq!(
        attr(root, "colonizer.pr.url"),
        Some(Value::from("https://github.com/acme/widgets/pull/7"))
    );
    let turn = by_name(&spans, "turn 1");
    assert_eq!(turn.parent_span_id, root.span_id);
    assert_eq!(attr(turn, "colonizer.origin"), Some(Value::from("user")));
    assert!(spans[..3].iter().all(|s| s.parent_span_id == turn.span_id));
    let denied = &spans[2];
    assert_eq!(attr(denied, "colonizer.denial.class"), Some(Value::from("egress")));
    assert_eq!(attr(denied, "error.type"), Some(Value::from("tool_error")));
    check_golden("traces-simple", to_json_value(&request(items)));
}

#[test]
fn the_data_v0_1_9_sessions_map_to_the_golden_trace() {
    let colonies = colonies();
    let r = run(&colonies);
    let mut state = Traces::default();
    let mut items = r.feed(
        &mut state,
        "a1b2c3d4",
        &fixture("data-v0.1.9-a1b2c3d4-events.jsonl"),
        &[outcome("a1b2c3d4", "merged", "2026-09-24T09:40:58Z")],
        NOW,
    );
    // A stopped colony silent for a day: its root goes out as `idle`.
    items.extend(r.feed(
        &mut state,
        "e5f60718",
        &fixture("data-v0.1.9-e5f60718-events.jsonl"),
        &[],
        NOW,
    ));
    let spans = spans(&items);
    assert_eq!(spans.len(), 2, "progress lines make no span; each colony gets its root");
    assert_eq!(attr(&spans[1], "colonizer.outcome"), Some(Value::from("idle")));
    assert_eq!(attr(&spans[1], "colonizer.origin"), Some(Value::from("burn_down")));
    check_golden("traces-data-v0.1.9", to_json_value(&request(items)));
}

#[test]
fn subagent_tool_spans_parent_to_the_subagent_which_parents_to_its_task_call() {
    let items = subagents();
    let spans = spans(&items);
    let task = |id: &str| {
        spans
            .iter()
            .find(|s| attr(s, "gen_ai.tool.call.id") == Some(Value::from(id)))
            .unwrap()
    };
    let explore = by_name(&spans, "subagent Explore");
    let task1 = task("toolu_task1");
    assert_eq!(attr(task1, "gen_ai.tool.name"), Some(Value::from("Task")));
    assert_eq!(explore.parent_span_id, task1.span_id, "the subagent sits under its Task call");
    assert_eq!(
        explore.start_time_unix_nano, task1.start_time_unix_nano,
        "it starts with the Task call"
    );
    for tool in ["execute_tool Read", "execute_tool Grep"] {
        assert_eq!(
            by_name(&spans, tool).parent_span_id,
            explore.span_id,
            "{tool} sits under the subagent"
        );
    }
    let turn1 = by_name(&spans, "turn 1");
    assert_eq!(task1.parent_span_id, turn1.span_id);

    // The background subagent outlives its turn and ends with its `subagent_end`.
    let worker = by_name(&spans, "subagent general-purpose");
    let task2 = task("toolu_task2");
    assert_eq!(worker.parent_span_id, task2.span_id);
    assert_eq!(worker.start_time_unix_nano, task2.start_time_unix_nano);
    assert!(worker.end_time_unix_nano > turn1.end_time_unix_nano);
    assert_eq!(by_name(&spans, "execute_tool Bash").parent_span_id, turn1.span_id);
    let unmatched: Vec<&Span> = spans
        .iter()
        .filter(|s| attr(s, "colonizer.unmatched") == Some(Value::Bool(true)))
        .collect();
    assert_eq!(unmatched.len(), 1, "only the unanswered Bash call closes unmatched");
    assert_eq!(attr(unmatched[0], "gen_ai.tool.call.id"), Some(Value::from("toolu_b1")));
    let b2 = task("toolu_b2");
    assert_eq!(b2.parent_span_id, worker.span_id);
    assert_eq!(
        attr(by_name(&spans, "turn 2"), "colonizer.origin"),
        Some(Value::from("watchdog"))
    );
    check_golden("traces-subagents", to_json_value(&request(items)));
}

#[test]
fn a_question_wait_maps_to_the_golden_trace() {
    let colonies = colonies();
    let r = run(&colonies);
    let items = r.feed(
        &mut Traces::default(),
        "9e57c0de",
        &fixture("question-events.jsonl"),
        &[outcome("9e57c0de", "no_changes", "2026-09-24T10:00:30Z")],
        NOW,
    );
    let spans = spans(&items);
    let turn = by_name(&spans, "turn 1");
    assert_eq!(
        turn.end_time_unix_nano - turn.start_time_unix_nano,
        10_000_000_000,
        "the turn spans the wait for both answers"
    );
    check_golden("traces-question", to_json_value(&request(items)));
}

#[test]
fn ids_are_deterministic_and_follow_the_design_formula() {
    let tid = trace_id(HOST, "c1a0dec0");
    let digest = ring::digest::digest(&ring::digest::SHA256, b"colonizer.trace.v1|host-1|c1a0dec0");
    assert_eq!(tid[..], digest.as_ref()[..16]);
    let mut input = tid.to_vec();
    input.extend_from_slice(b"turn1");
    let digest = ring::digest::digest(&ring::digest::SHA256, &input);
    assert_eq!(span_id(tid, SpanKind::Turn, "1")[..], digest.as_ref()[..8]);
    assert_eq!(
        simple(),
        simple(),
        "two runs over the same lines give the same spans, ids included"
    );
    let spans = spans(&simple());
    let ids: BTreeSet<&Vec<u8>> = spans.iter().map(|s| &s.span_id).collect();
    assert_eq!(ids.len(), spans.len(), "no two spans share an id");
    assert!(spans.iter().all(|s| s.trace_id == tid.to_vec()));
}

#[test]
fn a_restart_mid_turn_resumes_with_the_same_ids_and_a_crash_replays_the_same_spans() {
    let colonies = colonies();
    let r = run(&colonies);
    let events = fixture("subagents-events.jsonl");
    let activity = [outcome("5ab0a9e7", "merged", "2026-09-24T10:01:00Z")];
    let whole = subagents();
    for cut in 1..events.len() {
        let mut state = Traces::default();
        let mut out = r.feed(&mut state, "5ab0a9e7", &events[..cut], &[], NOW);
        // Committed to `state.json` and read back by the next process.
        let mut extra = BTreeMap::new();
        state.store(&mut extra, "dest");
        let json = serde_json::to_vec(&extra).unwrap();
        let reloaded: BTreeMap<String, Value> = serde_json::from_slice(&json).unwrap();
        let mut resumed = Traces::load(&reloaded, "dest");
        assert_eq!(resumed, state);
        // A crash after the ack, before the commit: the same lines over the same state again.
        let mut replay = resumed.clone();
        let rest = r.feed(&mut resumed, "5ab0a9e7", &events[cut..], &activity, NOW);
        let again = r.feed(&mut replay, "5ab0a9e7", &events[cut..], &activity, NOW);
        assert_eq!(again, rest, "cut at line {cut}");
        assert_eq!(replay, resumed, "cut at line {cut}");
        out.extend(rest);
        assert_eq!(out, whole, "cut at line {cut}");
    }
    assert!(Traces::load(&BTreeMap::new(), "dest").colonies.is_empty());
}

#[test]
fn turn_deltas_sum_to_the_last_cumulative_turn_end() {
    let spans = spans(&subagents());
    let turns: Vec<&Span> = spans.iter().filter(|s| s.name.starts_with("turn ")).collect();
    assert_eq!(turns.len(), 2);
    let sum = |key: &str| -> i64 { turns.iter().map(|t| attr(t, key).and_then(|v| v.as_i64()).unwrap()).sum() };
    assert_eq!(sum("gen_ai.usage.input_tokens"), 1540);
    assert_eq!(sum("gen_ai.usage.output_tokens"), 360);
    assert_eq!(sum("gen_ai.usage.cache_read.input_tokens"), 9000);
    assert_eq!(sum("gen_ai.usage.cache_creation.input_tokens"), 900);
    let cost: f64 = turns
        .iter()
        .map(|t| attr(t, "colonizer.cost_usd").and_then(|v| v.as_f64()).unwrap())
        .sum();
    assert!((cost - 0.8).abs() < 1e-9, "{cost}");
    let root = by_name(&spans, "invoke_agent acme/widgets");
    assert_eq!(attr(root, "gen_ai.usage.input_tokens"), Some(Value::from(1540)));
    assert_eq!(attr(turns[0], "gen_ai.response.model"), Some(Value::from("claude-opus-5")));
}

fn at(ts: &str) -> u64 {
    ts_nanos(&serde_json::json!({ "ts": ts })).unwrap()
}

#[test]
fn the_root_goes_out_once_across_stop_resume_and_merge() {
    let mut colonies = colonies();
    let r = run(&colonies);
    let events = fixture("subagents-events.jsonl");
    let mut state = Traces::default();
    let mut out = r.feed(
        &mut state,
        "5ab0a9e7",
        &events[..14],
        &[
            outcome("5ab0a9e7", "suspended", "2026-09-24T10:00:14Z"),
            outcome("5ab0a9e7", "restored", "2026-09-24T10:00:14Z"),
            outcome("5ab0a9e7", "stopped", "2026-09-24T10:00:14Z"),
        ],
        at("2026-09-24T12:00:00Z"),
    );
    assert!(
        spans(&out).iter().all(|s| !s.parent_span_id.is_empty()),
        "stopped is not an outcome"
    );
    out.extend(r.feed(&mut state, "5ab0a9e7", &events[14..], &[], at("2026-09-24T13:00:00Z")));
    out.extend(r.feed(
        &mut state,
        "5ab0a9e7",
        &[],
        &[outcome("5ab0a9e7", "merged", "2026-09-24T13:00:00Z")],
        at("2026-09-24T13:00:01Z"),
    ));
    // A late outcome, and a day of silence after it, change nothing.
    out.extend(r.feed(
        &mut state,
        "5ab0a9e7",
        &[],
        &[outcome("5ab0a9e7", "closed", "2026-09-24T14:00:00Z")],
        at("2026-09-26T13:00:00Z"),
    ));
    let all = spans(&out);
    let roots: Vec<&Span> = all.iter().filter(|s| s.parent_span_id.is_empty()).collect();
    assert_eq!(roots.len(), 1);
    assert_eq!(attr(roots[0], "colonizer.outcome"), Some(Value::from("merged")));
    let names: Vec<&str> = roots[0].events.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["colonizer.suspended", "colonizer.restored", "colonizer.stopped"]);

    // Stopped and silent for a day: the root goes out as idle, and a later merge sends no second.
    colonies.get_mut("5ab0a9e7").unwrap().status = Some("stopped".into());
    let r = run(&colonies);
    let mut state = Traces::default();
    let mut out = r.feed(&mut state, "5ab0a9e7", &events[..3], &[], at("2026-09-24T12:00:00Z"));
    assert!(out.is_empty());
    out.extend(r.feed(&mut state, "5ab0a9e7", &[], &[], at("2026-09-25T10:00:03Z")));
    out.extend(r.feed(
        &mut state,
        "5ab0a9e7",
        &[],
        &[outcome("5ab0a9e7", "merged", "2026-09-25T11:00:00Z")],
        at("2026-09-25T11:00:01Z"),
    ));
    let all = spans(&out);
    assert_eq!(all.len(), 2, "the open turn closes incomplete, then the root: {all:?}");
    assert_eq!(attr(&all[0], "colonizer.span.incomplete"), Some(Value::Bool(true)));
    assert_eq!(attr(&all[1], "colonizer.outcome"), Some(Value::from("idle")));
}

#[test]
fn a_root_waits_for_its_colony_events_to_be_read_to_the_end() {
    let colonies = colonies();
    let r = run(&colonies);
    let builder = r.builder();
    let events = fixture("subagents-events.jsonl");
    let mut state = Traces::default();
    let mut out = Vec::new();
    for line in &events[..12] {
        builder.feed(&mut state, Source::Events, Some("5ab0a9e7"), line, &digest(line), &mut out);
    }
    let merged = outcome("5ab0a9e7", "merged", "2026-09-24T10:01:00Z");
    builder.feed(&mut state, Source::Activity, None, &merged, &digest(&merged), &mut out);
    builder.settle(&mut state, &|_| false, &[], NOW, &mut out);
    assert!(
        spans(&out).iter().all(|s| !s.parent_span_id.is_empty()),
        "no root while lines are unread"
    );
    for line in &events[12..] {
        builder.feed(&mut state, Source::Events, Some("5ab0a9e7"), line, &digest(line), &mut out);
    }
    builder.settle(&mut state, &|_| true, &[], NOW, &mut out);
    assert_eq!(out, subagents());
}

#[test]
fn a_deleted_colony_gets_its_root_and_its_state_is_dropped() {
    let colonies = colonies();
    let r = run(&colonies);
    let builder = r.builder();
    let mut state = Traces::default();
    let mut out = Vec::new();
    for line in &fixture("subagents-events.jsonl")[..5] {
        builder.feed(&mut state, Source::Events, Some("5ab0a9e7"), line, &digest(line), &mut out);
    }
    builder.settle(&mut state, &|_| true, &["5ab0a9e7".to_string()], NOW, &mut out);
    let spans = spans(&out);
    assert_eq!(attr(spans.last().unwrap(), "colonizer.outcome"), Some(Value::from("deleted")));
    let incomplete = spans
        .iter()
        .filter(|s| attr(s, "colonizer.span.incomplete") == Some(Value::Bool(true)))
        .count();
    assert_eq!(incomplete, 3, "the turn, the Task call and the subagent close incomplete");
    assert!(state.colonies.is_empty());
}

#[test]
fn sampling_keeps_or_drops_whole_colonies() {
    let colonies = colonies();
    let mut none = run(&colonies);
    none.ratio = 0.0;
    let events = fixture("subagents-events.jsonl");
    let activity = [outcome("5ab0a9e7", "merged", "2026-09-24T10:01:00Z")];
    let mut state = Traces::default();
    assert!(none.feed(&mut state, "5ab0a9e7", &events, &activity, NOW).is_empty());
    assert!(state.colonies.is_empty(), "an unsampled colony holds no state");
    let all = run(&colonies);
    assert_eq!(
        all.feed(&mut Traces::default(), "5ab0a9e7", &events, &activity, NOW),
        subagents()
    );

    // The ratio is the share of trace ids kept, decided by the trace id alone.
    let kept = (0..2000)
        .filter(|i| sampled(trace_id(HOST, &format!("c{i:07}")), 0.25))
        .count();
    assert!((400..600).contains(&kept), "{kept} of 2000 at 0.25");
    assert!(sampled([0; 16], 0.000_001));
    assert!(!sampled([0xff; 16], 0.999_999));
    assert!(!sampled([0; 16], f64::NAN));
}

#[test]
fn open_spans_past_the_cap_are_evicted_oldest_first() {
    let colonies = colonies();
    let r = run(&colonies);
    let mut lines =
        vec![serde_json::json!({"type": "user_message", "id": "initial", "text": "go", "ts": "2026-09-24T10:00:00Z"})];
    for i in 0..MAX_OPEN_SPANS + 3 {
        lines.push(
            serde_json::json!({"type": "tool_call", "tool_call_id": format!("t{i}"), "name": "Read", "ts": "2026-09-24T10:00:01Z"}),
        );
    }
    let mut state = Traces::default();
    let out = r.feed(&mut state, "5ab0a9e7", &lines, &[], NOW);
    let spans = spans(&out);
    let evicted: Vec<Value> = spans.iter().filter_map(|s| attr(s, "gen_ai.tool.call.id")).collect();
    assert_eq!(
        evicted,
        [Value::from("t0"), Value::from("t1"), Value::from("t2"), Value::from("t3")],
        "the four oldest of 4 099 tool calls beside the open turn"
    );
    assert!(spans.iter().all(|s| attr(s, "colonizer.evicted") == Some(Value::Bool(true))));
    assert_eq!(state.colonies["5ab0a9e7"].open.len(), MAX_OPEN_SPANS);
}

/// Appends `text` to every string of `value`, except the ids and keys that hold the tree together.
pub(crate) fn fill(value: &mut Value, text: &str) {
    match value {
        Value::String(s) => *s = format!("{s} {text}"),
        Value::Array(items) => items.iter_mut().for_each(|v| fill(v, text)),
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if !matches!(
                    key.as_str(),
                    "type" | "tool_call_id" | "question_id" | "id" | "ts" | "kind" | "colony"
                ) {
                    fill(v, text);
                }
            }
        }
        _ => {}
    }
}

/// Every request `items` makes, in both encodings, checked for the content canary and every secret.
pub(crate) fn assert_no_canary(items: &[Item], canaries: &Canaries) {
    for encoding in [Encoding::Protobuf, Encoding::Json] {
        let policy = policy();
        let resource = policy.resource(&[("service.name", "colonizer".into())]);
        let mut batcher = Batcher::new(
            &resource,
            BatchConfig {
                encoding,
                ..BatchConfig::default()
            },
        );
        for item in items.iter().cloned() {
            batcher.push(item);
        }
        for request in batcher.finish().requests {
            let bytes = request.encode(encoding);
            crate::testkit::assert_absent(&bytes, [canaries.content.as_str()], "content canary in a span");
            crate::testkit::assert_absent(&bytes, canaries.secrets(), "secret in a span");
        }
    }
}

#[test]
fn no_content_or_secret_reaches_a_span() {
    let canaries = Canaries::new();
    let secrets: Vec<&str> = canaries.all.iter().map(|c| c.text.as_str()).collect();
    let text = format!("{} {}", canaries.content, secrets.join(" "));
    let mut colonies = colonies();
    for p in colonies.values_mut() {
        p.agent = format!("claude_code {text}");
        p.origin = Some(text.clone());
    }
    let r = run(&colonies);
    let mut items = Vec::new();
    // Question and answer text, exec-policy commands, path-policy paths, verification commands and
    // files, finding titles and reasons, boundary details.
    for (colony, file) in [
        ("c1a0dec0", "claude-code-events.jsonl"),
        ("5ab0a9e7", "subagents-events.jsonl"),
        ("9e57c0de", "question-events.jsonl"),
        ("6a7e3a11", "host-chain-events.jsonl"),
    ] {
        let mut lines = fixture(file);
        for line in &mut lines {
            fill(line, &text);
        }
        let mut gateway = fixture("gateway.jsonl");
        for line in &mut gateway {
            fill(line, &text);
        }
        items.extend(r.feed_all(
            &mut Traces::default(),
            colony,
            &lines,
            &gateway,
            &[outcome(colony, "merged", "2026-09-24T11:00:00Z")],
            NOW,
        ));
    }
    let names: BTreeSet<String> = spans(&items).iter().map(|s| s.name.clone()).collect();
    for name in ["question", "host_step verification", "host_step path_policy", "chat"] {
        assert!(names.contains(name), "{name} was not built: {names:?}");
    }
    assert!(spans(&items).len() > 40, "the canaries did not stop the spans being built");
    assert_no_canary(&items, &canaries);
}

/// Maps a synthetic 200 MB `events.jsonl` (generated here, never committed) and reports the peak
/// RSS growth. Run with `cargo test -p colonizer-observability --release -- --ignored large_`.
#[test]
#[ignore]
fn large_colony_maps_in_bounded_memory() {
    fn rss_kib() -> u64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
    }
    let colonies = colonies();
    let r = run(&colonies);
    let builder = r.builder();
    let mut state = Traces::default();
    let before = rss_kib();
    let mut peak = before;
    let (mut bytes, mut spans_out) = (0usize, 0usize);
    let filler = "y".repeat(1500);
    let mut i = 0u64;
    while bytes < 200 * 1024 * 1024 {
        let ts = format!("2026-09-24T{:02}:{:02}:{:02}.000Z", (i / 3600) % 24, (i / 60) % 60, i % 60);
        let lines = [
            serde_json::json!({"type": "user_message", "id": format!("m{i}"), "text": filler, "ts": ts}),
            serde_json::json!({"type": "tool_call", "tool_call_id": format!("t{i}"), "name": "Bash", "input": {"command": filler}, "ts": ts}),
            serde_json::json!({"type": "tool_result", "tool_call_id": format!("t{i}"), "output": filler, "ts": ts}),
            serde_json::json!({"type": "turn_end", "is_error": false, "cost_usd": i as f64, "model_usage": {"m": {"input_tokens": i, "output_tokens": i, "cache_read_tokens": 0, "cache_write_tokens": 0}}, "ts": ts}),
        ];
        let mut out = Vec::new();
        for line in &lines {
            bytes += line.to_string().len() + 1;
            builder.feed(&mut state, Source::Events, Some("5ab0a9e7"), line, &digest(line), &mut out);
        }
        spans_out += out.len();
        if i.is_multiple_of(1000) {
            peak = peak.max(rss_kib());
        }
        i += 1;
    }
    peak = peak.max(rss_kib());
    let growth_mib = peak.saturating_sub(before) as f64 / 1024.0;
    eprintln!(
        "mapped {} MiB, {spans_out} spans, peak RSS growth {growth_mib:.1} MiB",
        bytes / (1024 * 1024)
    );
    assert!(growth_mib <= 64.0, "{growth_mib} MiB");
}

#[test]
fn the_host_chain_and_gateway_retries_map_to_the_golden_trace() {
    let colonies = colonies();
    let r = run(&colonies);
    let items = r.feed_all(
        &mut Traces::default(),
        "6a7e3a11",
        &fixture("host-chain-events.jsonl"),
        &fixture("gateway.jsonl"),
        &[outcome("6a7e3a11", "merged", "2026-09-24T11:01:00Z")],
        NOW,
    );
    let spans = spans(&items);
    let root = by_name(&spans, "invoke_agent acme/widgets");
    let verification = by_name(&spans, "host_step verification");
    assert_eq!(verification.parent_span_id, root.span_id, "host steps hang off the root");
    assert_eq!(
        verification.end_time_unix_nano - verification.start_time_unix_nano,
        6_000_000_000,
        "a verification spans its run"
    );
    assert_eq!(attr(verification, "colonizer.verdict"), Some(Value::from("contradicted")));
    assert_eq!(attr(verification, "colonizer.verify.files_changed"), Some(Value::from(2)));
    assert_eq!(verification.status.as_ref().unwrap().code, StatusCode::Error as i32);
    let policy = by_name(&spans, "host_step path_policy");
    assert_eq!(attr(policy, "colonizer.path_policy.access"), Some(Value::from("write")));
    assert_eq!(attr(policy, "colonizer.path_policy.policy"), Some(Value::from("protected")));
    assert_eq!(policy.start_time_unix_nano, policy.end_time_unix_nano, "instantaneous");
    let jev = by_name(&spans, "host_step jev_ladder");
    for key in [
        "colonizer.jev.kept",
        "colonizer.jev.dropped_results",
        "colonizer.jev.dropped_calls",
    ] {
        assert_eq!(attr(jev, key), Some(Value::from(1)), "{key}");
    }
    let steps = spans.iter().filter(|s| s.name.starts_with("host_step ")).count();
    assert_eq!(steps, 11, "every chain line is one step");

    // Gateway retries: one client span per attempt, the failure's code as its error type.
    let chats: Vec<&Span> = spans.iter().filter(|s| s.name.starts_with("chat")).collect();
    let names: Vec<&str> = chats.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["chat qwen/qwen3-coder", "chat qwen/qwen3-coder", "chat qwen3-coder"]);
    assert!(chats.iter().all(|s| s.parent_span_id == root.span_id));
    assert!(
        chats
            .iter()
            .all(|s| s.kind == crate::proto::trace::v1::span::SpanKind::Client as i32)
    );
    assert_eq!(attr(chats[0], "error.type"), Some(Value::from("upstream_error")));
    assert_eq!(attr(chats[0], "colonizer.fallback"), Some(Value::Bool(true)));
    assert_eq!(attr(chats[1], "error.type"), None);
    assert_eq!(attr(chats[1], "gen_ai.usage.output_tokens"), Some(Value::from(420)));
    assert_eq!(chats[1].end_time_unix_nano - chats[1].start_time_unix_nano, 2_400_000_000);
    assert_eq!(attr(chats[2], "error.type"), Some(Value::from("queue_full")));
    check_golden("traces-host-chain", to_json_value(&request(items)));
}

#[test]
fn questions_say_who_answered_and_a_restart_keeps_an_open_one() {
    let colonies = colonies();
    let r = run(&colonies);
    let events = fixture("question-events.jsonl");
    let activity = [outcome("9e57c0de", "no_changes", "2026-09-24T10:00:30Z")];
    let whole = r.feed(&mut Traces::default(), "9e57c0de", &events, &activity, NOW);
    let spans = spans(&whole);
    let questions: Vec<&Span> = spans.iter().filter(|s| s.name == "question").collect();
    assert_eq!(questions.len(), 2);
    let turn = by_name(&spans, "turn 1");
    assert!(questions.iter().all(|q| q.parent_span_id == turn.span_id));
    assert_eq!(attr(questions[0], "colonizer.answered_by"), Some(Value::from("user")));
    assert_eq!(
        attr(questions[0], "colonizer.question.risk"),
        Some(Value::from("workspace_write"))
    );
    assert_eq!(attr(questions[0], "colonizer.question.options"), Some(Value::from(2)));
    assert_eq!(attr(questions[1], "colonizer.answered_by"), Some(Value::from("autonomy")));
    assert_eq!(
        attr(questions[1], "colonizer.question.kind"),
        Some(Value::from("exec_policy"))
    );
    assert_eq!(attr(questions[1], "colonizer.question.blocking"), Some(Value::Bool(true)));
    assert_eq!(
        questions[0].end_time_unix_nano - questions[0].start_time_unix_nano,
        2_000_000_000,
        "from the question to its answer"
    );

    // A restart while each question is open: same span id, closed by the answer after it.
    for cut in [3, 8] {
        let mut state = Traces::default();
        let mut out = r.feed(&mut state, "9e57c0de", &events[..cut], &[], NOW);
        let open = &state.colonies["9e57c0de"].open;
        assert!(open.iter().any(|o| o.at.kind == SpanKindName::Question), "cut {cut}");
        let mut extra = BTreeMap::new();
        state.store(&mut extra, "dest");
        let mut resumed = Traces::load(&serde_json::from_slice(&serde_json::to_vec(&extra).unwrap()).unwrap(), "dest");
        out.extend(r.feed(&mut resumed, "9e57c0de", &events[cut..], &activity, NOW));
        assert_eq!(out, whole, "cut {cut}");
    }

    // Never answered: closed at the root, unanswered; a lead agent's question outlives its turn.
    let lines: Vec<Value> = events[..4].iter().chain(&events[10..]).cloned().collect();
    let items = r.feed(&mut Traces::default(), "9e57c0de", &lines, &activity, NOW);
    let spans = self::spans(&items);
    let q = by_name(&spans, "question");
    assert_eq!(attr(q, "colonizer.unanswered"), Some(Value::Bool(true)));
    assert_eq!(attr(q, "colonizer.span.incomplete"), Some(Value::Bool(true)));
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["turn 1", "question", "invoke_agent acme/widgets"]);
}

#[test]
fn a_fifty_thousand_tool_call_colony_fits_the_trace_budget() {
    let colonies = colonies();
    let r = run(&colonies);
    let builder = r.builder();
    let mut state = Traces::default();
    let mut out = Vec::new();
    let (turns, per_turn) = (50, 1000);
    for t in 0..turns {
        let ts = |i: u64| format!("2026-09-24T{:02}:{:02}:{:02}.000Z", 10 + t / 60, t % 60, i % 60);
        let mut lines = vec![serde_json::json!({"type": "user_message", "id": format!("m{t}"), "text": "go", "ts": ts(0)})];
        for i in 0..per_turn {
            let id = format!("toolu_{t:02}_{i:04}");
            lines.push(serde_json::json!({"type": "tool_call", "tool_call_id": id, "name": "Bash", "ts": ts(i)}));
            lines.push(
                serde_json::json!({"type": "tool_result", "tool_call_id": id, "output": "ok", "is_error": false, "ts": ts(i)}),
            );
        }
        lines.push(serde_json::json!({"type": "turn_end", "is_error": false, "cost_usd": t as f64, "ts": ts(59)}));
        for line in &lines {
            builder.feed(&mut state, Source::Events, Some("5ab0a9e7"), line, &digest(line), &mut out);
        }
    }
    let merged = outcome("5ab0a9e7", "merged", "2026-09-24T11:00:00Z");
    builder.feed(&mut state, Source::Activity, None, &merged, &digest(&merged), &mut out);
    builder.settle(&mut state, &|_| true, &[], NOW, &mut out);

    let total: u64 = out.iter().map(encoded_size).sum();
    assert!(total <= DEFAULT_MAX_TRACE_BYTES, "{total} bytes");
    assert_eq!(total, state.colonies["5ab0a9e7"].bytes, "the running count is the bytes sent");
    let spans = spans(&out);
    let turn_spans: Vec<&Span> = spans.iter().filter(|s| s.name.starts_with("turn ")).collect();
    assert_eq!(turn_spans.len(), turns as usize, "every turn is sent");
    let tools = spans.iter().filter(|s| s.name == "execute_tool Bash").count() as i64;
    assert!(tools > 10_000 && tools < 50_000, "{tools} tool spans sent");
    let suppressed: i64 = turn_spans
        .iter()
        .filter_map(|t| attr(t, "colonizer.spans_suppressed.execute_tool").and_then(|v| v.as_i64()))
        .sum();
    assert_eq!(
        suppressed + tools,
        50_000,
        "every tool call is either sent or counted on its turn"
    );
    let root = by_name(&spans, "invoke_agent acme/widgets");
    assert_eq!(attr(root, "colonizer.trace_budget_exhausted"), Some(Value::Bool(true)));
    assert_eq!(attr(root, "colonizer.trace.dropped_spans"), Some(Value::from(suppressed)));
    assert_eq!(
        attr(root, "colonizer.spans_suppressed.execute_tool"),
        Some(Value::from(suppressed))
    );
    // The detail spans stop at 90 %: the rest is the turns' and the root's.
    let details: u64 = out
        .iter()
        .zip(&spans)
        .filter(|(_, s)| s.name == "execute_tool Bash")
        .map(|(i, _)| encoded_size(i))
        .sum();
    assert!(details <= DEFAULT_MAX_TRACE_BYTES / 10 * 9);
}

#[test]
fn span_attributes_stay_under_tempos_two_kib() {
    let policy = Policy::new(
        PolicyConfig {
            max_attribute_bytes: 8192,
            ..PolicyConfig::default()
        },
        ContentGate::closed(),
        None,
    );
    let long = "a".repeat(6000);
    let items = [policy
        .span(SpanKind::ExecuteTool, "Bash")
        .attr("gen_ai.tool.name", long.as_str(), Tier::Structure)
        .finish()];
    let span = &spans(&items)[0];
    let value = attr(span, "gen_ai.tool.name").unwrap();
    let value = value.as_str().unwrap();
    assert!(value.len() <= crate::policy::SPAN_ATTRIBUTE_BYTES, "{}", value.len());
    assert!(value.ends_with("more bytes)"), "cut with the marker");
    assert_eq!(attr(span, "colonizer.truncated"), Some(Value::Bool(true)));
    // A log record keeps the configured cap.
    let log = policy
        .log(Source::Gateway)
        .attr("model", long.as_str(), Tier::Structure)
        .finish();
    let crate::batch::Record::Log(record) = &log.0 else { panic!() };
    let kept = serde_json::to_value(record.attributes[0].value.as_ref().unwrap()).unwrap();
    assert!(kept["stringValue"].as_str().unwrap().len() > 4000);
}
