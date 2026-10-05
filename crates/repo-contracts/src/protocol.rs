//! The agent-event contract fixture and the origin schema (moved from `protocol.rs`).

use colonizer_harness::contract::{EventShape, Origin, event_shape};
use serde_json::Value;

/// The runner's contract fixture: every line must deserialise and land on the variant the
/// harness dispatches on — or, for the forwarded-only types, deliberately on the catch-all.
/// This is the seam between the JS runner and this enum, so drift fails a test on both sides.
#[test]
fn every_fixture_event_deserialises_and_lands_on_its_variant() {
    let fixture = include_str!("../../../modules/agents/claude-code/test/fixtures/events.jsonl");
    let events: Vec<EventShape> = fixture.lines().map(event_shape).collect();

    // The events the harness acts on, with the fields the dispatch reads.
    assert_eq!(
        events[0],
        EventShape::Status {
            state: "idle",
            detail: None
        }
    );
    assert_eq!(
        events[1],
        EventShape::UserMessage {
            id: "initial".into(),
            text: "Fix the issue".into()
        }
    );
    assert!(matches!(
        &events[2],
        EventShape::Status {
            state,
            detail: None
        } if *state == "working"
    ));
    // The agent-session id the harness keeps on the record (issue #562): a resumable runner
    // announces it at init, before the log line naming the same session.
    assert!(matches!(
        &events[3],
        EventShape::AgentSession { session_id } if !session_id.is_empty()
    ));
    assert!(matches!(
        &events[9],
        EventShape::Question { question_id, question_count, message_id: Some(message_id), .. }
            if question_id == "toolu_ask" && message_id == "msg_1" && *question_count == 1
    ));
    assert!(matches!(
        &events[10],
        EventShape::Status {
            state,
            detail: None
        } if *state == "waiting_for_answer"
    ));
    assert!(matches!(
        &events[11],
        EventShape::QuestionAnswered { question_id, response: None, .. } if question_id == "toolu_ask"
    ));
    assert!(matches!(
        &events[16],
        EventShape::MemoryProposal { scope: Some(scope), tags, .. }
            if scope == "repo" && tags == &["workspace".to_string()]
    ));
    assert!(matches!(
        &events[17],
        EventShape::Finding { title, evidence, .. } if !title.is_empty() && !evidence.is_empty()
    ));
    assert!(matches!(
        &events[25],
        EventShape::Status {
            state,
            detail: None
        } if *state == "idle"
    ));
    assert!(matches!(
        &events[26],
        EventShape::Status {
            state,
            detail: None
        } if *state == "exited"
    ));
    match &events[24] {
        EventShape::TurnEnd {
            is_error,
            cost_usd,
            model_usage: Some(usage),
            ..
        } => {
            assert!(!is_error);
            assert!(cost_usd.is_some_and(|cost| cost > 0.0));
            assert!(usage.is_object());
        }
        other => panic!("the turn that ends the fixture is a turn_end, got {other:?}"),
    }
    // The masked read the runner reports (issue #647): the attempt, with the tool that made it.
    assert_eq!(
        events[22],
        EventShape::PathPolicy {
            access: "read".into(),
            policy: "masked".into(),
            path: ".env".into(),
            tool: "Read".into(),
        }
    );

    // The egress refusal the runner reports for the watchdog (issue #609), after its result.
    assert_eq!(
        events[20],
        EventShape::Boundary {
            kind: "egress_denied".into(),
            control: "egress".into(),
            target: Some("github.com".into()),
        }
    );

    // The forwarded-only types land on the catch-all on purpose: the browser is their consumer.
    assert_eq!(events[4], EventShape::Other, "log");
    assert_eq!(events[5], EventShape::Other, "model_changed");
    assert_eq!(events[6], EventShape::Other, "assistant_text_delta");
    assert_eq!(events[8], EventShape::Other, "assistant_text");
    assert_eq!(events[13], EventShape::Other, "thinking");
    assert_eq!(events[14], EventShape::Other, "tool_call");
    assert_eq!(events[15], EventShape::Other, "tool_result");
    assert_eq!(events[18], EventShape::Other, "tool_call");
    assert_eq!(events[19], EventShape::Other, "tool_result with a denial");
    assert_eq!(events[21], EventShape::Other, "tool_call for the masked read");
    assert_eq!(events[23], EventShape::Other, "tool_result of the masked read");
}

/// The schema's `#/$defs/origin` enum is this vocabulary's second hand-kept side: a variant
/// added here without the schema — or a wire spelling changed on one side only — fails here, the
/// same seam the fixture test above pins for the event types themselves.
#[test]
fn the_origin_variants_are_exactly_the_schemas_origin_enum() {
    let schema: Value = serde_json::from_str(include_str!("../../../docs/agent-events.schema.json")).unwrap();
    let mut schema_enum: Vec<&str> = schema["$defs"]["origin"]["enum"]
        .as_array()
        .expect("the schema defines #/$defs/origin as an enum")
        .iter()
        .map(|v| v.as_str().expect("an enum of strings"))
        .collect();
    schema_enum.sort_unstable();
    let mut variants: Vec<&str> = [
        Origin::User,
        Origin::Agent,
        Origin::Subagent,
        Origin::Watchdog,
        Origin::Autonomy,
        Origin::BurnDown,
        Origin::Redteam,
        Origin::Notify,
        Origin::System,
    ]
    .iter()
    .map(|o| o.as_str())
    .collect();
    variants.sort_unstable();
    assert_eq!(variants, schema_enum);
}
