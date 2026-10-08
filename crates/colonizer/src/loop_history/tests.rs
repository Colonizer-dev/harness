use super::*;
use crate::sessions::tests::colony;
use chrono::TimeZone;

fn at(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
}

fn record(id: &str, when: DateTime<Utc>, outcome: Option<Outcome>, colonies: &[&str]) -> RunRecord {
    RunRecord {
        loop_id: id.into(),
        at: when,
        finished_at: None,
        trigger: "schedule".into(),
        outcome,
        summary: "a run".into(),
        counts: BTreeMap::new(),
        colonies: colonies.iter().map(|c| c.to_string()).collect(),
    }
}

fn session(id: &str, status: SessionStatus, cost: f64, routed: f64) -> Session {
    let mut s = colony("acme", status);
    s.id = id.into();
    s.cost_usd = Some(cost);
    s.routed_cost_usd = Some(routed);
    s
}

fn field(v: &Value, path: &str) -> Value {
    path.split('.').fold(v.clone(), |v, k| match k.parse::<usize>() {
        Ok(i) => v[i].clone(),
        Err(_) => v[k].clone(),
    })
}

#[test]
fn the_history_has_one_bucket_per_day_with_outcomes_and_cost() {
    let now = at(2026, 10, 7, 12);
    let records = vec![
        record("merge-train", at(2026, 10, 7, 9), Some(Outcome::Ok), &["s1"]),
        record("merge-train", at(2026, 10, 7, 10), Some(Outcome::Failed), &[]),
        record("merge-train", at(2026, 10, 5, 10), Some(Outcome::Partial), &["s2", "s3"]),
        // Older than the range: left out of a 3-day answer.
        record("merge-train", at(2026, 10, 1, 10), Some(Outcome::Ok), &["s1"]),
    ];
    let sessions = vec![
        session("s1", SessionStatus::Merged, 1.5, 0.5),
        session("s2", SessionStatus::PrOpened, 2.0, 0.0),
        session("s3", SessionStatus::Running, 0.25, 0.0),
    ];
    let v = view("merge-train", &records, &sessions, 3, 0, now);
    assert_eq!(field(&v, "buckets.0.day"), "2026-10-05");
    assert_eq!(field(&v, "buckets.1.runs"), 0, "a quiet day is a zero bucket, not a gap");
    assert_eq!(field(&v, "buckets.2.runs"), 2);
    assert_eq!(field(&v, "buckets.2.ok"), 1);
    assert_eq!(field(&v, "buckets.2.failed"), 1);
    assert_eq!(field(&v, "buckets.2.cost_usd"), 2.0);
    assert_eq!(field(&v, "buckets.0.partial"), 1);
    assert_eq!(field(&v, "buckets.0.colonies"), 2);
    assert_eq!(field(&v, "buckets.0.cost_usd"), 2.25);
    assert_eq!(field(&v, "totals.runs"), 3);
    assert_eq!(field(&v, "totals.cost_usd"), 4.25);
    assert_eq!(field(&v, "runs.0.at"), "2026-10-07T10:00:00Z", "runs list newest first");
    assert_eq!(field(&v, "runs.0.outcome"), "failed");
    assert_eq!(field(&v, "last.at"), "2026-10-07T10:00:00Z");
    let narrow = view("merge-train", &records, &sessions, 1, 0, now);
    assert_eq!(
        field(&narrow, "last.at"),
        "2026-10-07T10:00:00Z",
        "the last run is the loop's latest whatever the range"
    );
    let quiet = view("merge-train", &records, &sessions, 1, 0, at(2026, 12, 1, 12));
    assert_eq!(field(&quiet, "totals.runs"), 0);
    assert_eq!(field(&quiet, "last.at"), "2026-10-07T10:00:00Z");
}

#[test]
fn a_colony_loop_run_takes_its_colonys_outcome() {
    let now = at(2026, 10, 7, 12);
    let records = vec![
        record("loop_1", at(2026, 10, 7, 1), None, &["done"]),
        record("loop_1", at(2026, 10, 7, 2), None, &["broke"]),
        record("loop_1", at(2026, 10, 7, 3), None, &["working"]),
        record("loop_1", at(2026, 10, 7, 4), None, &["empty"]),
    ];
    let sessions = vec![
        session("done", SessionStatus::PrOpened, 1.0, 0.0),
        session("broke", SessionStatus::Failed, 0.5, 0.0),
        session("working", SessionStatus::Running, 0.1, 0.0),
        session("empty", SessionStatus::NoChanges, 0.2, 0.0),
    ];
    let v = view("loop_1", &records, &sessions, 1, 0, now);
    assert_eq!(field(&v, "totals.ok"), 1);
    assert_eq!(field(&v, "totals.failed"), 1);
    assert_eq!(field(&v, "totals.running"), 1);
    assert_eq!(field(&v, "totals.skipped"), 1);
}

#[test]
fn days_are_cut_at_the_callers_midnight() {
    let now = at(2026, 10, 7, 23);
    // 23:00 UTC is already the 8th at UTC+2.
    let records = vec![record("x", at(2026, 10, 7, 23), Some(Outcome::Ok), &[])];
    let utc = view("x", &records, &[], 2, 0, now);
    assert_eq!(field(&utc, "to"), "2026-10-07");
    assert_eq!(field(&utc, "buckets.1.runs"), 1);
    let east = view("x", &records, &[], 2, 120, now);
    assert_eq!(field(&east, "to"), "2026-10-08");
    assert_eq!(field(&east, "buckets.1.runs"), 1);
    assert_eq!(field(&east, "buckets.0.runs"), 0);
}

#[test]
fn records_are_kept_for_ninety_days_and_capped_per_loop() {
    let now = at(2026, 10, 7, 12);
    let mut records = vec![
        record("a", now - Duration::days(91), Some(Outcome::Ok), &[]),
        record("a", now - Duration::days(89), Some(Outcome::Ok), &[]),
    ];
    prune(&mut records, now);
    assert_eq!(records.len(), 1);
    let mut many: Vec<RunRecord> = (0..MAX_PER_LOOP + 5)
        .map(|i| {
            record(
                "b",
                now - Duration::minutes((MAX_PER_LOOP + 5 - i) as i64),
                Some(Outcome::Ok),
                &[],
            )
        })
        .collect();
    many.push(record("c", now, Some(Outcome::Ok), &[]));
    prune(&mut many, now);
    assert_eq!(many.iter().filter(|r| r.loop_id == "b").count(), MAX_PER_LOOP);
    assert_eq!(many.iter().filter(|r| r.loop_id == "c").count(), 1);
    assert!(many[0].at < many[1].at, "the oldest go, the rest keep their order");
}

#[test]
fn a_merge_run_with_red_and_merged_is_partial_and_idle_is_skipped() {
    use crate::merge_loop::{Action, Item, RepoReport, Report};
    let item = |action, session: &str| Item {
        session: session.into(),
        pr_url: "https://github.com/a/b/pull/1".into(),
        title: "t".into(),
        action,
        reason: "r".into(),
    };
    let mut report = Report {
        summary: "merged 3, red 2".into(),
        repos: vec![RepoReport {
            repo: "a/b".into(),
            items: vec![
                item(Action::Merged, "m"),
                item(Action::Red, "r"),
                item(Action::Waiting, "w"),
                item(Action::Resolving, "res"),
            ],
            ..RepoReport::default()
        }],
        ..Report::default()
    };
    let rec = from_merge_loop(&report);
    assert_eq!(rec.outcome, Some(Outcome::Partial));
    assert_eq!(rec.counts["merged"], 1);
    assert_eq!(rec.counts["red"], 1);
    assert_eq!(rec.counts["waiting"], 1);
    assert_eq!(
        rec.colonies,
        vec!["res".to_string()],
        "only the colonies it resumed are priced"
    );
    report.repos[0].items = vec![item(Action::Waiting, "w")];
    assert_eq!(from_merge_loop(&report).outcome, Some(Outcome::Skipped));
    report.repos[0].items = vec![item(Action::Red, "r")];
    assert_eq!(from_merge_loop(&report).outcome, Some(Outcome::Failed));
}

#[tokio::test]
async fn the_store_survives_a_restart_and_the_endpoint_serves_known_loops_only() {
    let root = std::env::temp_dir().join(format!("colonizer-loop-history-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    record_now(&app).await;
    let reloaded = HistoryStore::new(&app.cfg.config_dir);
    assert_eq!(reloaded.of(MERGE_TRAIN).await.len(), 1);
    let Json(v) = history(
        State(app.clone()),
        Path(MERGE_TRAIN.to_string()),
        Query(HistoryQuery {
            days: Some(500),
            tz_offset_minutes: None,
        }),
        None,
    )
    .await
    .unwrap();
    assert_eq!(v["days"], 90, "days are clamped to the retention");
    assert_eq!(v["totals"]["runs"], 1);
    let missing = history(State(app.clone()), Path("nope".into()), Query(HistoryQuery::default()), None).await;
    assert_eq!(missing.err().map(|e| e.status()), Some(StatusCode::NOT_FOUND));
    // Disk cleanup is a real loop in the store, so it answers even before its first run.
    assert!(
        history(
            State(app),
            Path(DISK_CLEANUP.to_string()),
            Query(HistoryQuery::default()),
            None
        )
        .await
        .is_ok()
    );
    let _ = std::fs::remove_dir_all(&root);
}

async fn record_now(app: &Shared) {
    super::record(app, record(MERGE_TRAIN, Utc::now(), Some(Outcome::Ok), &[])).await;
}
