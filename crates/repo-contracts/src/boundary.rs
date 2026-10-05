//! The boundary event against its schema (moved from `boundary.rs`).

use colonizer_harness::contract::{BOUNDARY_KINDS, boundary_event};
use serde_json::Value;

/// Every kind the schema names is one the harness reads, and the other way round.
#[test]
fn the_kinds_are_exactly_the_schemas_enum() {
    let schema: Value = serde_json::from_str(include_str!("../../../docs/agent-events.schema.json")).unwrap();
    let kinds: Vec<&str> = schema["$defs"]["boundary"]["properties"]["kind"]["enum"]
        .as_array()
        .expect("boundary.kind is an enum")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(kinds, BOUNDARY_KINDS);
    let required: Vec<&str> = schema["$defs"]["boundary"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let written = boundary_event("sandbox_denied", "read_only_mount", "x", None);
    for field in required {
        assert!(written.get(field).is_some(), "the written event lacks the required {field}");
    }
}
