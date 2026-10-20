//! The shared duplicate rules (issue #832), apart from any launch path. The paths themselves are
//! checked against each other in `supply_chain_loop/tests.rs` (issue #821's scenario).

use super::*;
use crate::claims::{ClaimKind, RemoteClaimInfo};
use crate::sessions::tests::colony;
use axum::response::IntoResponse;

fn held(id: &str, status: SessionStatus) -> Session {
    let mut s = colony("acme", status);
    s.id = id.into();
    s
}

fn target(package: &str, advisory: &str) -> SupplyChainTarget {
    SupplyChainTarget::new(package, advisory)
}

fn supply(package: &str, advisory: &str) -> Work {
    Work {
        repo: "acme/repo".into(),
        supply_chain: vec![target(package, advisory)],
        ..Work::default()
    }
}

fn on_issue(issue: u64) -> Work {
    Work {
        repo: "acme/repo".into(),
        issue: Some(issue),
        ..Work::default()
    }
}

fn refusal(v: Verdict) -> Refusal {
    match v {
        Verdict::Refuse(r) => r,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn an_issue_hold_refuses_names_the_holder_and_can_be_queued_behind() {
    let mut holder = held("first", SessionStatus::PrOpened);
    holder.issue = Some(7);
    holder.pr_url = Some("https://github.com/acme/repo/pull/9".into());
    let sessions = vec![holder];

    let r = refusal(check(&sessions, &[], &on_issue(7), false, false));
    assert_eq!(r.holder.kind, HoldKind::Issue);
    assert!(r.is_held_by("first"));
    assert_eq!(r.holder.pr_url.as_deref(), Some("https://github.com/acme/repo/pull/9"));
    assert_eq!(r.holder.status.as_deref(), Some("pr_opened"));
    assert!(r.holder.queueable, "an issue hold can be waited for");
    assert!(
        r.message.contains("colony first is already on #7") && r.message.contains("allow_duplicate"),
        "{}",
        r.message
    );
    assert_eq!(
        check(&sessions, &[], &on_issue(7), false, true),
        Verdict::Queue("first".into())
    );
    assert_eq!(
        check(&sessions, &[], &on_issue(7), true, true),
        Verdict::Allow,
        "allow_duplicate wins"
    );
    assert_eq!(
        check(&sessions, &[], &on_issue(8), false, false),
        Verdict::Allow,
        "another issue is free"
    );
}

#[test]
fn a_supply_chain_target_collides_on_package_and_advisory_or_a_missing_advisory() {
    let mut holder = held("holder", SessionStatus::Running);
    holder.supply_chain = Some(target("lodash", "ghsa-1"));
    let sessions = vec![holder];

    let r = refusal(check(&sessions, &[], &supply("Lodash", "GHSA-1"), false, false));
    assert_eq!(r.holder.kind, HoldKind::SupplyChain);
    assert!(!r.holder.queueable, "a supply-chain fix is not a queue");
    assert_eq!(r.holder.what, "lodash / ghsa-1");
    assert!(r.message.contains("colony holder") && r.message.contains("lodash / ghsa-1"));
    assert!(
        matches!(
            check(&sessions, &[], &supply("lodash", "ghsa-1"), false, true),
            Verdict::Refuse(_)
        ),
        "queue_behind_holder does not turn a supply-chain hold into a wait"
    );
    assert_eq!(
        check(&sessions, &[], &supply("lodash", "ghsa-2"), false, false),
        Verdict::Allow
    );
    assert_eq!(
        check(&sessions, &[], &supply("left-pad", "ghsa-1"), false, false),
        Verdict::Allow
    );
    // A finding with no advisory (a yanked release) is on every advisory of its package.
    assert!(matches!(
        check(&sessions, &[], &supply("lodash", ""), false, false),
        Verdict::Refuse(_)
    ));
    // Another repository is other work.
    let elsewhere = Work {
        repo: "acme/other".into(),
        ..supply("lodash", "ghsa-1")
    };
    assert_eq!(check(&sessions, &[], &elsewhere, false, false), Verdict::Allow);
}

#[test]
fn a_parked_fix_holds_its_target_but_not_an_issue_and_a_finished_one_holds_nothing() {
    let mut parked = held("parked", SessionStatus::Parked);
    parked.issue = Some(7);
    parked.supply_chain = Some(target("lodash", "ghsa-1"));
    let sessions = vec![parked];
    assert!(matches!(
        check(&sessions, &[], &supply("lodash", "ghsa-1"), false, false),
        Verdict::Refuse(_)
    ));
    assert_eq!(check(&sessions, &[], &on_issue(7), false, false), Verdict::Allow);
    for status in [
        SessionStatus::Merged,
        SessionStatus::Closed,
        SessionStatus::Failed,
        SessionStatus::Stopped,
    ] {
        let mut done = held("done", status);
        done.supply_chain = Some(target("lodash", "ghsa-1"));
        assert_eq!(
            check(&[done], &[], &supply("lodash", "ghsa-1"), false, false),
            Verdict::Allow,
            "{status:?}"
        );
    }
}

#[test]
fn a_colony_claims_its_targets_its_loop_record_or_its_hand_off_title() {
    let mut loop_colony = held("loop", SessionStatus::Running);
    loop_colony.origin = Some("supply-chain:cargo".into());
    loop_colony.supply_chain_targets = vec![target("hyper", "rustsec-1"), target("futures-util", "")];
    assert_eq!(claim(&loop_colony, &[]).len(), 2);

    // An older loop colony carries nothing; the loop's record says what it was given.
    let mut old = held("old", SessionStatus::Running);
    old.origin = Some("supply-chain:cargo".into());
    let recorded = vec![Recorded {
        session: "old".into(),
        targets: vec![target("hyper", "rustsec-1")],
    }];
    assert_eq!(claim(&old, &recorded), vec![target("hyper", "rustsec-1")]);
    // With no record either, it is on everything its own origin dispatches, and nothing else.
    let cargo = Work {
        origin: Some("supply-chain:cargo".into()),
        ..supply("serde", "rustsec-9")
    };
    assert!(matches!(
        check(std::slice::from_ref(&old), &[], &cargo, false, false),
        Verdict::Refuse(_)
    ));
    let npm = Work {
        origin: Some("supply-chain:npm".into()),
        ..supply("serde", "rustsec-9")
    };
    assert_eq!(check(std::slice::from_ref(&old), &[], &npm, false, false), Verdict::Allow);

    // A Packages hand-off from before targets were recorded claims its package by title.
    let mut hand = held("hand", SessionStatus::Running);
    hand.issue_title = "Supply chain: hyper".into();
    assert_eq!(claim(&hand, &[]), vec![target("hyper", "")]);
    // ...but a fresh launch's own title is never read as a target.
    assert!(Work::of(&hand).supply_chain.is_empty());
}

#[test]
fn the_merge_time_overlap_reads_the_same_rule() {
    let mut a = held("a", SessionStatus::Merged);
    a.supply_chain_targets = vec![target("hyper", "rustsec-1"), target("tokio", "rustsec-2")];
    let mut b = held("b", SessionStatus::Running);
    b.supply_chain = Some(target("Hyper", "RUSTSEC-1"));
    assert!(supply_chain_overlap(&a, &b));
    b.supply_chain = Some(target("hyper", "rustsec-3"));
    assert!(!supply_chain_overlap(&a, &b));
}

#[test]
fn a_remote_claim_names_the_host_colony_and_pull_request() {
    let info = RemoteClaimInfo {
        kind: ClaimKind::PullRequest,
        detail: "https://github.com/acme/repo/pull/4".into(),
        host: Some("archlinux (f88)".into()),
        colony: Some("33577107".into()),
        merged: false,
        updated: None,
        author: None,
    };
    let r = remote_refusal(&info, 7, true);
    assert_eq!(r.holder.kind, HoldKind::RemoteClaim);
    assert!(r.is_held_by("33577107"));
    assert_eq!(r.holder.host.as_deref(), Some("archlinux (f88)"));
    assert_eq!(r.holder.pr_url.as_deref(), Some("https://github.com/acme/repo/pull/4"));
    assert!(
        r.message.contains("allow_duplicate") && r.message.contains("unreachable"),
        "{}",
        r.message
    );
}

#[tokio::test]
async fn the_409_body_carries_the_holder_beside_the_message() {
    let mut holder = held("first", SessionStatus::Running);
    holder.issue = Some(7);
    let r = refusal(check(&[holder], &[], &on_issue(7), false, false));
    let message = r.message.clone();
    let res = r.into_error().into_response();
    assert_eq!(res.status(), axum::http::StatusCode::CONFLICT);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], serde_json::json!(message));
    assert_eq!(body["duplicate"]["kind"], "issue");
    assert_eq!(body["duplicate"]["colony"], "first");
    assert_eq!(body["duplicate"]["status"], "running");
    assert_eq!(body["duplicate"]["queueable"], true);
}

/// Issue #1284: issue_held_by matches repos case-insensitively.
#[test]
fn issue_held_by_matches_repo_case_insensitively() {
    let mut holder = held("first", SessionStatus::Running);
    holder.repo = "Acme/App".to_string();
    holder.issue = Some(7);
    let result = issue_held_by(&[holder.clone()], "acme/app", 7);
    assert!(result.is_some(), "case-insensitive repo match");
    let result2 = issue_held_by(&[holder.clone()], "ACME/APP", 7);
    assert!(result2.is_some(), "uppercase repo match");
    let mut holder2 = held("second", SessionStatus::Running);
    holder2.repo = "acme/app".to_string();
    holder2.issue = Some(7);
    let result3 = issue_held_by(&[holder2.clone(), holder.clone()], "acme/app", 7);
    assert_eq!(result3.map(|s| s.id), Some("second".to_string()), "first non-waiter wins");
}

#[test]
fn remote_holder_includes_claimed_by_and_claimed_at() {
    use chrono::Utc;
    let now = Utc::now();
    let info = RemoteClaimInfo {
        kind: ClaimKind::PullRequest,
        detail: "https://github.com/acme/repo/pull/4".into(),
        host: Some("archlinux (f88)".into()),
        colony: Some("abc123".into()),
        merged: false,
        updated: Some(now),
        author: Some("someone".into()),
    };
    let holder = remote_holder(&info, 7);
    assert_eq!(holder.claimed_by.as_deref(), Some("someone"));
    assert_eq!(holder.claimed_at.as_ref(), Some(&now.to_rfc3339()));
    assert!(!holder.stale_hint, "PR claim is not stale");
}

#[test]
fn remote_holder_stale_hint_for_any_label_only_claim() {
    use chrono::{Duration, Utc};
    let now = Utc::now();
    let old_time = now - Duration::hours(25);
    let fresh_time = now - Duration::hours(12);

    // Label-only claim (old) has stale hint.
    let mut info = RemoteClaimInfo {
        kind: ClaimKind::Label,
        detail: crate::claims::CLAIM_LABEL.to_string(),
        host: None,
        colony: Some("abc123".into()),
        merged: false,
        updated: Some(old_time),
        author: Some("someone".into()),
    };
    let holder = remote_holder(&info, 7);
    assert!(holder.stale_hint, "label-only claim (any age) has stale hint");

    // Label-only claim (fresh) also has stale hint.
    info.updated = Some(fresh_time);
    let holder = remote_holder(&info, 7);
    assert!(holder.stale_hint, "label-only claim (fresh) has stale hint");

    // Non-label-only claims never have stale hint.
    info.kind = ClaimKind::Branch;
    info.detail = "colonizer/issue-7-abc123".to_string();
    let holder = remote_holder(&info, 7);
    assert!(!holder.stale_hint, "branch claim is not stale");
}

#[test]
fn remote_holder_omits_stale_hint_and_dates_when_json_serialized_if_not_set() {
    let info = RemoteClaimInfo {
        kind: ClaimKind::PullRequest,
        detail: "https://github.com/acme/repo/pull/4".into(),
        host: Some("archlinux (f88)".into()),
        colony: Some("abc123".into()),
        merged: false,
        updated: None,
        author: None,
    };
    let holder = remote_holder(&info, 7);
    let json = serde_json::to_value(&holder).unwrap();
    assert!(!json.get("claimed_by").is_some(), "claimed_by should be omitted when None");
    assert!(!json.get("claimed_at").is_some(), "claimed_at should be omitted when None");
    assert_eq!(json.get("stale_hint"), None, "stale_hint=false should be omitted");
}
