//! Issue #969: the loop when GitHub CI cannot run — local checks against a scripted GitHub and a
//! scripted microVM.

use super::*;

const BILLING: &str = "the Actions spending limit was reached";

fn unavailable_main() -> Result<MainCi, String> {
    Ok(MainCi::Unavailable {
        sha: "tip0".into(),
        reason: BILLING.into(),
    })
}

/// A pull request whose every check GitHub refused to start.
fn refused(n: u64) -> Result<Reading, String> {
    let mut r = reading(n, Mergeability::Clean, CiState::Failure, 0);
    r.unavailable = Some(BILLING.into());
    Ok(r)
}

fn local_on(fake: &Fake) {
    *fake.local.borrow_mut() = Some(LocalChecks::On {
        commands: vec!["npm ci".into(), "npm test".into()],
        source: "detected from the stack".into(),
    });
}

fn cfg_local() -> Settings {
    Settings {
        local_checks: vec!["acme".into()],
        ..cfg()
    }
}

#[tokio::test]
async fn a_refused_pull_request_merges_once_its_local_checks_pass() {
    let fake = Fake::new().main(vec![unavailable_main()]).pr("s1", vec![refused(1)]);
    local_on(&fake);
    *fake.local_runs.borrow_mut() = VecDeque::from([LocalRun::Passed { base_sha: "tip0".into() }]);
    let mut mem = BTreeMap::new();
    let report = run(&fake, &cfg_local(), &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "status head1 pending",
            "local s1 head1 [npm ci; npm test]",
            "status head1 success",
            "merge s1 Change 1 (#1)",
        ]
    );
    let merged = item(&report, "s1");
    assert_eq!(merged.action, Action::Merged);
    assert!(
        merged.reason.contains("local checks") && merged.reason.contains(BILLING),
        "{}",
        merged.reason
    );
    assert!(
        fake.log.borrow().iter().any(|l| l == "config opted_in=true"),
        "the loop's list opts the org in"
    );
    // CI that could not run on main is not red: nothing was re-run and no colony was sent.
    assert!(report.repos[0].heal.is_empty());
}

#[tokio::test]
async fn a_failing_local_check_never_merges_and_is_not_rerun_on_the_same_head() {
    let fake = Fake::new().main(vec![unavailable_main()]).pr("s1", vec![refused(1)]);
    local_on(&fake);
    *fake.local_runs.borrow_mut() = VecDeque::from([LocalRun::Failed {
        base_sha: "tip0".into(),
        command: "npm test".into(),
        tail: "1 failing\n".into(),
    }]);
    let mut mem = BTreeMap::new();
    let report = run(&fake, &cfg_local(), &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(
        fake.writes(),
        vec![
            "status head1 pending",
            "local s1 head1 [npm ci; npm test]",
            "status head1 failure"
        ]
    );
    let red = item(&report, "s1");
    assert_eq!(red.action, Action::Red);
    assert!(
        red.reason.contains("`npm test` failed") && red.reason.contains("1 failing"),
        "{}",
        red.reason
    );
    // Same head, same main: the failure stands without another run.
    fake.log.borrow_mut().clear();
    let again = run(&fake, &cfg_local(), &[session("s1", 1)], &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert_eq!(item(&again, "s1").action, Action::Red);
}

#[tokio::test]
async fn a_pull_request_whose_ci_ran_and_failed_is_never_merged_by_this_path() {
    let mut genuine = reading(1, Mergeability::Clean, CiState::Failure, 0);
    genuine.failing = vec![FailingCheck {
        name: "unit".into(),
        run_id: None,
    }];
    let fake = Fake::new().main(vec![unavailable_main()]).pr("s1", vec![Ok(genuine)]);
    local_on(&fake);
    let mut mem = BTreeMap::new();
    let report = run(&fake, &cfg_local(), &[session("s1", 1)], &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert_eq!(item(&report, "s1").action, Action::Red);
    // An `unavailable` reading on a head whose CI is pending is never acted on either.
    let mut pending = reading(2, Mergeability::Clean, CiState::Pending, 0);
    pending.unavailable = Some(BILLING.into());
    assert_eq!(
        Engine {
            ops: &fake,
            cfg: &cfg_local(),
            sessions: &[],
            guards: Guards {
                allowed_authors: vec!["colonizer-settlers".into()],
                forbidden: Vec::new(),
            },
            dry: false,
            calls: 0,
            quiet: Duration::from_secs(600),
        }
        .plan(&pending),
        Plan::WaitCi
    );
}

#[tokio::test]
async fn without_local_checks_unavailable_ci_holds_the_repository() {
    let fake = Fake::new().main(vec![unavailable_main()]).pr("s1", vec![refused(1)]);
    let mut mem = BTreeMap::new();
    let report = run(&fake, &cfg(), &[session("s1", 1)], &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    let held = item(&report, "s1");
    assert!(
        held.reason.contains("GitHub CI could not run") && held.reason.contains("off"),
        "{}",
        held.reason
    );
    assert!(fake.log.borrow().iter().any(|l| l == "config opted_in=false"));
}

#[tokio::test]
async fn main_moving_during_the_checks_holds_the_merge_and_a_dry_run_only_says_what_it_would_do() {
    let fake = Fake::new()
        .main(vec![
            unavailable_main(),
            Ok(MainCi::Unavailable {
                sha: "tip1".into(),
                reason: BILLING.into(),
            }),
        ])
        .pr("s1", vec![refused(1)]);
    local_on(&fake);
    *fake.local_runs.borrow_mut() = VecDeque::from([LocalRun::Passed { base_sha: "tip0".into() }]);
    let mut mem = BTreeMap::new();
    let report = run(&fake, &cfg_local(), &[session("s1", 1)], &mut mem, false).await;
    assert!(!fake.writes().iter().any(|w| w.starts_with("merge")), "{:?}", fake.writes());
    assert!(item(&report, "s1").reason.contains("moved"));

    let dry = Fake::new().main(vec![unavailable_main()]).pr("s1", vec![refused(1)]);
    local_on(&dry);
    let report = run(&dry, &cfg_local(), &[session("s1", 1)], &mut BTreeMap::new(), true).await;
    assert!(dry.writes().is_empty(), "{:?}", dry.writes());
    assert!(item(&report, "s1").reason.contains("would run its local checks"));
}

/// Issue #972: entering CI-unavailable mode is announced once, staying in it is not, and leaving it
/// — main's CI running green again — is announced once.
#[tokio::test]
async fn entering_and_leaving_ci_unavailable_mode_is_announced_once_each() {
    let fake = Fake::new()
        .main(vec![unavailable_main(), unavailable_main(), main_green(), main_green()])
        .pr("s1", vec![Ok(reading(1, Mergeability::Conflicted, CiState::Failure, 0))]);
    let mut mem = BTreeMap::new();
    let mut notices = Vec::new();
    for _ in 0..4 {
        let r = run(&fake, &cfg_local(), &[session("s1", 1)], &mut mem, false).await;
        notices.push(r.notices);
    }
    assert_eq!(notices[0].len(), 1);
    assert!(
        notices[0][0].starts_with("acme/web: GitHub CI can't run (the Actions spending limit"),
        "{:?}",
        notices[0]
    );
    assert!(notices[1].is_empty(), "still unavailable: said already");
    assert_eq!(
        notices[2],
        vec!["acme/web: GitHub CI runs again; the merge train is back to merging on CI"]
    );
    assert!(notices[3].is_empty());
    assert_eq!(mem["acme/web"].ci_unavailable, None);
    // A run that never read main's CI changes nothing either way.
    assert_eq!(ci_edge("a/b", Some("x"), None, false), (None, Some("x".to_string())));
}
