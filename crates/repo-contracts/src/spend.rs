//! The shared spend fixture summed on both sides of the language split (moved from `spend.rs`).

use colonizer_harness::contract::{short_id, spend_fixture_rows, spend_fixture_totals, spend_journal_path};

/// Reconciliation with the node half of issue #296: the shared fixture is read and summed the
/// exact way `GET /api/spend/history` reads and sums a `spend.jsonl`, and the sums must equal
/// the constants `scripts/test/colony-report.test.mjs` asserts for the same file (its
/// `--costs` reconciliation test). Change the fixture, or either side's expectations, and the
/// other two follow.
#[test]
fn the_shared_spend_fixture_sums_the_same_on_both_sides_of_the_language_split() {
    let root = std::env::temp_dir().join(format!("colonizer-spend-{}", short_id()));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(
        spend_journal_path(&data_dir),
        include_str!("../../../scripts/test/fixtures/spend-costs.jsonl"),
    )
    .unwrap();

    // A fixed window wide enough for every day the fixture names (2026-09-14 and 2026-09-15),
    // so the test does not age out as the real calendar moves.
    let totals = spend_fixture_totals(&data_dir);
    assert_eq!(totals.days, 2, "the two days the fixture mentions, oldest first");
    // Every dollar the fixture carries is a binary fraction, so the f64 sums are exact; the
    // epsilon keeps the comparison honest should the fixture ever grow one that is not.
    assert!(
        (totals.cost_usd - 1.1875).abs() < 1e-9,
        "usage-row dollars, chat and legacy included, got {}",
        totals.cost_usd
    );
    assert!(
        (totals.routed_cost_usd - 0.375).abs() < 1e-9,
        "the routed/gateway dollars, got {}",
        totals.routed_cost_usd
    );
    assert_eq!(totals.input_tokens, 1300);
    assert_eq!(totals.output_tokens, 305);
    assert_eq!(totals.cache_read_tokens, 200);
    assert_eq!(totals.cache_write_tokens, 0);

    // The new-format rows name their colony and harness; the fixture's one legacy row and its
    // chat row still parse, unnamed.
    let rows = spend_fixture_rows(&data_dir);
    assert_eq!(rows.len(), 17, "the torn line is skipped, everything else reads");
    assert!(
        rows.iter()
            .filter(|r| r.session.as_deref() == Some("claudeaa"))
            .all(|r| r.agent.as_deref() == Some("claude-code")),
        "the claudeaa rows carry their harness"
    );
    let legacy = rows.iter().find(|r| r.session.is_none() && r.day == "2026-09-14").unwrap();
    assert_eq!(legacy.agent, None, "the fixture's legacy row parses unnamed");
    let chat = rows.iter().find(|r| r.org == "chat").unwrap();
    assert_eq!(chat.session, None);
    assert_eq!(chat.agent, None);

    let _ = std::fs::remove_dir_all(root);
}
