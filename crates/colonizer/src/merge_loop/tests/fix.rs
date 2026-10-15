//! Issue #1054: a red pull request goes back to its own colony — with the failing jobs' logs, one
//! attempt per head, the base first, CI that never ran never briefed, and a person past the last
//! attempt — against a scripted GitHub.

use super::*;
use crate::merge_loop::fix::Started;

const BILLING: &str = "the Actions spending limit was reached";

fn cfg_fix() -> Settings {
    Settings { fix_red: true, ..cfg() }
}

/// A pull request whose `unit` check ran and failed, on Actions run 555.
fn red(n: u64) -> Result<Reading, String> {
    let mut r = reading(n, Mergeability::Clean, CiState::Failure, 0);
    r.failing = failing_checks_from(&json!([
        {"name": "unit", "conclusion": "FAILURE", "detailsUrl": "https://github.com/acme/web/actions/runs/555/job/1"},
    ]));
    Ok(r)
}

/// A pull request whose every check GitHub refused to start.
fn refused(n: u64) -> Result<Reading, String> {
    let mut r = red(n).unwrap();
    r.unavailable = Some(BILLING.into());
    Ok(r)
}

fn fix_note(fake: &Fake) -> String {
    fake.fix_notes.borrow().last().cloned().expect("no fix was started")
}

#[tokio::test]
async fn a_red_pull_request_is_resumed_with_the_failing_logs_and_one_attempt_per_head() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1)]);
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
    // The failing job's log is read, then the colony is resumed with the brief.
    assert_eq!(fake.writes(), vec!["log 555".to_string(), "fix s1".to_string()]);
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Fixing);
    assert_eq!(i.reason, "fixing checks (attempt 1/2): unit");
    let note = fix_note(&fake);
    for needle in [
        "https://github.com/acme/web/pull/1",
        "`main`",
        "- unit",
        "### unit (run 555)",
        "error[E0308]: mismatched types",
        "Never skip, delete or weaken a test or check",
        "Run the failing checks locally",
        "the same pull request",
    ] {
        assert!(note.contains(needle), "{needle}: {note}");
    }
    let entry = &mem["acme/web"].fixing["https://github.com/acme/web/pull/1"];
    assert_eq!(entry.attempts, 1);
    assert_eq!(entry.head_sha.as_deref(), Some("head1"));
    assert_eq!(entry.checks, vec!["unit".to_string()]);

    // Still red on the same head: not sent again — a new push gets a new try.
    fake.log.borrow_mut().clear();
    let again = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert!(item(&again, "s1").reason.contains("already sent back for this exact head"));

    // While the fix is in flight the colony is watched, not read, and reported as fixing.
    let mut sessions = vec![session("s1", 1)];
    sessions[0].status = SessionStatus::Running;
    fake.log.borrow_mut().clear();
    let working = run(&fake, &cfg_fix(), &sessions, &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert!(!fake.log.borrow().iter().any(|l| l == "read s1"));
    assert_eq!(item(&working, "s1").action, Action::Fixing);
    assert_eq!(item(&working, "s1").reason, "fixing checks (attempt 1/2): unit");
}

#[tokio::test]
async fn a_check_red_on_the_base_too_is_left_for_the_base_and_never_sent_to_the_pull_request() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1)]);
    fake.base_red.borrow_mut().push("unit".into());
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Red);
    assert!(
        i.reason.contains("unit failing on main too") && i.reason.contains("the base goes first"),
        "{}",
        i.reason
    );
    assert!(mem["acme/web"].fixing.is_empty(), "nothing was sent: {:#?}", mem);
    assert!(!i.reason.contains("needs-human"), "no person was asked: {}", i.reason);
}

#[tokio::test]
async fn checks_github_refused_to_start_are_never_a_failure_to_fix() {
    // The whole loop: a refused reading is CI unavailable, not red, and nothing is sent to fix it.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![refused(1)]);
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert!(
        item(&r, "s1").reason.contains("GitHub CI could not run"),
        "{}",
        item(&r, "s1").reason
    );
    assert!(fake.fix_notes.borrow().is_empty());

    // And the gate itself, should a refused reading ever plan as red.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1)]);
    let cfg = cfg_fix();
    let mut engine = Engine {
        ops: &fake,
        cfg: &cfg,
        sessions: &[],
        guards: Guards {
            allowed_authors: vec!["colonizer-settlers".into()],
            forbidden: Vec::new(),
        },
        dry: false,
        calls: 0,
        quiet: Duration::from_secs(600),
    };
    let reading = refused(1).unwrap();
    let mut mem = RepoMemory::default();
    let Ok((action, why)) = engine.red(&session("s1", 1), &reading, "main", &mut mem).await else {
        panic!("the run stopped");
    };
    assert_eq!(action, Action::Red);
    assert!(
        why.contains("GitHub CI could not run") && why.contains("not a failure to fix"),
        "{why}"
    );
    assert!(fake.fix_notes.borrow().is_empty(), "{:?}", fake.writes());
}

#[tokio::test]
async fn past_its_fix_attempts_a_red_pull_request_is_labelled_and_left_to_a_person() {
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![red(1), red(1), red(2), red(2)]);
    let mut mem = BTreeMap::new();
    let mut runs = Vec::new();
    for _ in 0..5 {
        fake.log.borrow_mut().clear();
        let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
        runs.push((fake.writes(), item(&r, "s1").clone()));
    }
    // Attempt 1, on head1.
    assert_eq!(runs[0].0, vec!["log 555".to_string(), "fix s1".to_string()]);
    assert_eq!(runs[0].1.reason, "fixing checks (attempt 1/2): unit");
    // The same head red again is not a new attempt.
    assert!(runs[1].0.is_empty(), "{:?}", runs[1]);
    assert!(runs[1].1.reason.contains("already sent back for this exact head"));
    // A new push is: attempt 2, on head2.
    assert_eq!(runs[2].0, vec!["log 555".to_string(), "fix s1".to_string()]);
    assert_eq!(runs[2].1.reason, "fixing checks (attempt 2/2): unit");
    // Out of attempts: labelled once, and never sent again.
    assert_eq!(runs[3].0, vec!["label s1 needs-human".to_string()]);
    assert!(
        runs[3].1.reason.contains("2 fix attempts did not turn the checks green"),
        "{}",
        runs[3].1.reason
    );
    assert!(runs[4].0.is_empty(), "labelled once: {:?}", runs[4]);
    assert!(runs[4].1.reason.contains("needs a person"), "{}", runs[4].1.reason);
    let entry = &mem["acme/web"].fixing["https://github.com/acme/web/pull/1"];
    assert_eq!(entry.attempts, 2);
    assert!(entry.labeled && entry.gave_up.is_some());
}

#[tokio::test]
async fn a_reclaimed_worktree_leaves_it_to_a_person_or_sends_the_brief_to_a_redo_colony() {
    // Redo colonies off (the default): labelled needs-human, with the worktree named.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1)]);
    *fake.fixes.borrow_mut() = VecDeque::from([Started::Gone("the colony's worktree is gone".into())]);
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "log 555".to_string(),
            "fix s1".to_string(),
            "label s1 needs-human".to_string()
        ]
    );
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Red);
    assert!(i.reason.contains("worktree was reclaimed"), "{}", i.reason);
    assert!(i.reason.contains("needs-human"), "{}", i.reason);
    assert!(mem["acme/web"].fixing["https://github.com/acme/web/pull/1"].gave_up.is_some());

    // Redo colonies on: the brief goes to a fresh redo colony instead.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1)]);
    *fake.fixes.borrow_mut() = VecDeque::from([Started::Gone("the colony's worktree is gone".into())]);
    let cfg = Settings {
        redo_on_conflict: true,
        ..cfg_fix()
    };
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg, &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "log 555".to_string(),
            "fix s1".to_string(),
            "dispatch fix-redo s1".to_string()
        ]
    );
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::RedoDispatched);
    assert!(
        i.reason.contains("worktree was reclaimed") && i.reason.contains("dispatched fix colony c0lony"),
        "{}",
        i.reason
    );
}

#[tokio::test]
async fn with_fix_red_off_a_red_pull_request_sits_red_for_a_person() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1)]);
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert!(
        !fake.log.borrow().iter().any(|l| l.starts_with("branch-failing")),
        "the base is not even read"
    );
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Red);
    assert_eq!(i.reason, "failing: unit");
    assert!(fake.fix_notes.borrow().is_empty());
    assert!(mem["acme/web"].fixing.is_empty());
}

#[tokio::test]
async fn a_fix_that_never_starts_burns_no_attempt_and_pins_no_head() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1), red(1)]);
    *fake.fixes.borrow_mut() = VecDeque::from([
        Started::Failed("the worktree has uncommitted changes".into()),
        Started::Resumed,
    ]);
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(fake.writes(), vec!["log 555".to_string(), "fix s1".to_string()]);
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Waiting);
    assert!(
        i.reason.contains("the fix could not start") && i.reason.contains("try again on the next run"),
        "{}",
        i.reason
    );
    assert!(
        mem["acme/web"].fixing.is_empty(),
        "no attempt burned, no head pinned: {:#?}",
        mem["acme/web"].fixing
    );

    // The next run is the first attempt, not the second: the one-per-head guard did not fire.
    fake.log.borrow_mut().clear();
    let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(fake.writes(), vec!["log 555".to_string(), "fix s1".to_string()]);
    assert_eq!(item(&r, "s1").reason, "fixing checks (attempt 1/2): unit");
    assert_eq!(mem["acme/web"].fixing["https://github.com/acme/web/pull/1"].attempts, 1);
}

#[tokio::test]
async fn a_failed_redo_dispatch_is_retried_next_run_too() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1), red(1)]);
    *fake.fixes.borrow_mut() = VecDeque::from([Started::Gone("the colony's worktree is gone".into())]);
    fake.dispatch_err.borrow_mut().push_back("the fleet is full".into());
    let cfg = Settings {
        redo_on_conflict: true,
        ..cfg_fix()
    };
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg, &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "log 555".to_string(),
            "fix s1".to_string(),
            "dispatch fix-redo s1".to_string()
        ]
    );
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Waiting);
    assert!(
        i.reason.contains("dispatching a fix colony failed") && i.reason.contains("try again on the next run"),
        "{}",
        i.reason
    );
    assert!(
        mem["acme/web"].fixing.is_empty(),
        "the next run retries: {:#?}",
        mem["acme/web"].fixing
    );

    // The next run is the first attempt, not the second.
    fake.fixes.borrow_mut().clear();
    fake.log.borrow_mut().clear();
    let r = run(&fake, &cfg, &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(item(&r, "s1").reason, "fixing checks (attempt 1/2): unit");
    assert_eq!(mem["acme/web"].fixing["https://github.com/acme/web/pull/1"].attempts, 1);
}

#[tokio::test]
async fn one_fix_per_repository_at_a_time() {
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![red(1)])
        .pr("s2", vec![red(2)]);
    let mut mem = BTreeMap::new();
    // The first red pull request is sent back.
    let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(fake.writes(), vec!["log 555".to_string(), "fix s1".to_string()]);
    assert_eq!(item(&r, "s1").reason, "fixing checks (attempt 1/2): unit");

    // While that colony works, a second red pull request in the same repository waits its turn.
    let mut sessions = vec![session("s1", 1), session("s2", 2)];
    sessions[0].status = SessionStatus::Running;
    fake.log.borrow_mut().clear();
    let r = run(&fake, &cfg_fix(), &sessions, &mut mem, false).await;
    assert!(fake.writes().is_empty(), "no second fix: {:?}", fake.writes());
    assert_eq!(item(&r, "s1").action, Action::Fixing);
    let i = item(&r, "s2");
    assert_eq!(i.action, Action::Waiting);
    assert!(
        i.reason.contains("colony s1 is fixing another pull request here") && i.reason.contains("one at a time"),
        "{}",
        i.reason
    );
    assert!(
        mem["acme/web"].fixing.len() == 1,
        "only the first is out: {:#?}",
        mem["acme/web"].fixing
    );
}

#[tokio::test]
async fn the_base_goes_first_even_when_ci_is_unavailable_too() {
    // At the gate, one reading that is both red on the base and refused by GitHub: the base-first
    // reason wins. (The whole loop never plans a refused reading as red at all.)
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![refused(1)]);
    fake.base_red.borrow_mut().push("unit".into());
    let cfg = cfg_fix();
    let mut engine = Engine {
        ops: &fake,
        cfg: &cfg,
        sessions: &[],
        guards: Guards {
            allowed_authors: vec!["colonizer-settlers".into()],
            forbidden: Vec::new(),
        },
        dry: false,
        calls: 0,
        quiet: Duration::from_secs(600),
    };
    let reading = refused(1).unwrap();
    let mut mem = RepoMemory::default();
    let Ok((action, why)) = engine.red(&session("s1", 1), &reading, "main", &mut mem).await else {
        panic!("the run stopped");
    };
    assert_eq!(action, Action::Red);
    assert!(
        why.contains("unit failing on main too") && why.contains("the base goes first"),
        "{why}"
    );
    assert!(!why.contains("GitHub CI could not run"), "{why}");
    assert!(fake.fix_notes.borrow().is_empty(), "{:?}", fake.writes());
    assert!(mem.fixing.is_empty());
}

#[tokio::test]
async fn a_dry_run_says_it_would_fix_and_starts_nothing() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![red(1)]);
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg_fix(), &[session("s1", 1)], &mut mem, true).await;
    assert!(fake.writes().is_empty(), "no log read, nothing sent: {:?}", fake.writes());
    assert!(fake.fix_notes.borrow().is_empty());
    assert!(
        mem["acme/web"].fixing.is_empty(),
        "no memory written: {:#?}",
        mem["acme/web"].fixing
    );
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Fixing);
    assert_eq!(i.action.word(true), "would fix checks");
    assert!(
        i.reason.contains("would resume the colony to fix them") && i.reason.contains("attempt 1/2"),
        "{}",
        i.reason
    );
}
