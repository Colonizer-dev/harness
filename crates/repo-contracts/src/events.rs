//! Origin resolution over the shipped contract fixtures (moved from `events.rs`).

use colonizer_harness::contract::{Origin, resolve_origin};
use serde_json::Value;

/// The contract fixtures, line by line, through the resolver: runner lines almost never land on
/// `system`, the host's own stamp — a writer reading as system by default is exactly what this
/// vocabulary exists to catch. Includes the v0.1.9 stored files, whose lines predate the field:
/// legacy lines resolve like any other runner line. (memory_proposal is the one body whose own
/// `origin` shares the key with the stamp, §6.2 — its lines keep the proposer's value there.)
#[test]
fn fixture_lines_resolve_to_real_origins_not_the_system_catch_all() {
    for fixture in [
        include_str!("../../../modules/agents/claude-code/test/fixtures/events.jsonl"),
        include_str!("../../../crates/colonizer/tests/fixtures/data-v0.1.9/sessions/a1b2c3d4/events.jsonl"),
        include_str!("../../../crates/colonizer/tests/fixtures/data-v0.1.9/sessions/e5f60718/events.jsonl"),
    ] {
        let lines: Vec<&str> = fixture.lines().filter(|l| !l.trim().is_empty()).collect();
        let system = lines
            .iter()
            .filter(|line| {
                serde_json::from_str::<Value>(line).is_ok_and(|event| resolve_origin(&event, None, false) == Origin::System)
            })
            .count();
        assert!(
            system * 20 <= lines.len(),
            "{system} of {} lines resolve to system — new writers must opt into a real origin",
            lines.len()
        );
    }
}
