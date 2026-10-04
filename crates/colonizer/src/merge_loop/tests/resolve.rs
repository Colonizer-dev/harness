//! Issue #968: conflicted pull requests resolved by resuming their colony, against a scripted
//! GitHub and a scripted host.

use super::*;
use crate::merge_loop::resolve::Started;

fn cfg_resolve() -> Settings {
    Settings {
        resolve_conflicts: true,
        ..cfg()
    }
}

fn conflicted(n: u64) -> Result<Reading, String> {
    Ok(reading(n, Mergeability::Conflicted, CiState::Success, 0))
}

fn main_at(sha: &str) -> Result<MainCi, String> {
    Ok(MainCi::Green { sha: sha.into() })
}

fn status(sessions: &mut [Session], id: &str, to: SessionStatus) {
    sessions.iter_mut().find(|s| s.id == id).unwrap().status = to;
}

/// The acceptance case: two colony pull requests touching the same file. The first merges; the
/// second goes DIRTY, its colony is resumed to resolve it, publishes the merge, and merges on a
/// later run — no person involved, and nothing rebased or force-pushed.
#[tokio::test]
async fn the_second_of_two_overlapping_pull_requests_is_resolved_and_merges_without_a_person() {
    let fake = Fake::new()
        .main(vec![main_green()])
        .pr("s1", vec![green(1)])
        .pr("s2", vec![conflicted(2), green(2)]);
    let mut sessions = vec![session("s1", 1), session("s2", 2)];
    let mut mem = BTreeMap::new();
    let first = run(&fake, &cfg_resolve(), &sessions, &mut mem, false).await;
    assert_eq!(fake.writes(), vec!["resolve s2 onto main", "merge s1 Change 1 (#1)"]);
    assert_eq!(item(&first, "s2").action, Action::Resolving);
    assert!(item(&first, "s2").reason.contains("src/shared.rs"));
    assert_eq!(mem["acme/web"].resolving["https://github.com/acme/web/pull/2"].attempts, 1);

    // The colony is resolving: reported, not read, not touched.
    status(&mut sessions, "s1", SessionStatus::Merged);
    status(&mut sessions, "s2", SessionStatus::Running);
    fake.log.borrow_mut().clear();
    let working = run(&fake, &cfg_resolve(), &sessions, &mut mem, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    assert!(!fake.log.borrow().iter().any(|l| l == "read s2"));
    assert_eq!(item(&working, "s2").action, Action::Resolving);

    // It published the merge to the same pull request, which now reads clean and green.
    status(&mut sessions, "s2", SessionStatus::PrOpened);
    fake.log.borrow_mut().clear();
    let done = run(&fake, &cfg_resolve(), &sessions, &mut mem, false).await;
    assert_eq!(fake.writes(), vec!["merge s2 Change 2 (#2)"]);
    assert_eq!(item(&done, "s2").action, Action::Merged);
    assert!(
        mem["acme/web"].resolving.is_empty(),
        "a clean reading ends the resolve record"
    );
}

#[tokio::test]
async fn one_attempt_per_base_commit_then_a_person_once_the_attempts_run_out() {
    let fake = Fake::new()
        .main(vec![main_at("tip0"), main_at("tip0"), main_at("tip1"), main_at("tip2")])
        .pr("s1", vec![conflicted(1)]);
    *fake.resolves.borrow_mut() = VecDeque::from([Started::Failed("boom".into())]);
    let cfg = Settings {
        resolve_attempts: 2,
        ..cfg_resolve()
    };
    let sessions = vec![session("s1", 1)];
    let mut mem = BTreeMap::new();
    let mut runs = Vec::new();
    for _ in 0..5 {
        fake.log.borrow_mut().clear();
        let r = run(&fake, &cfg, &sessions, &mut mem, false).await;
        runs.push((fake.writes(), item(&r, "s1").clone()));
    }
    assert_eq!(runs[0].0, vec!["resolve s1 onto main"]);
    assert!(
        runs[1].0.is_empty() && runs[1].1.reason.contains("waits for main to move"),
        "{:?}",
        runs[1]
    );
    assert_eq!(runs[2].0, vec!["resolve s1 onto main"], "main moved: the second attempt");
    assert_eq!(
        runs[3].0,
        vec!["label s1 needs-human"],
        "out of attempts: labelled, not tried again"
    );
    assert_eq!(runs[3].1.action, Action::NeedsRedo);
    assert!(runs[4].0.is_empty(), "labelled once: {:?}", runs[4].0);
    assert!(runs[4].1.reason.contains("needs a person"));
}

#[tokio::test]
async fn a_conflict_the_repository_keeps_for_people_is_labelled_and_never_resolved() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![conflicted(1)]);
    *fake.resolves.borrow_mut() = VecDeque::from([Started::NeedsHuman(
        "it conflicts in migrations/0042.sql, which .colonizer/merge.toml says never to auto-resolve".into(),
    )]);
    let mut mem = BTreeMap::new();
    let r = run(&fake, &cfg_resolve(), &[session("s1", 1)], &mut mem, false).await;
    assert_eq!(fake.writes(), vec!["resolve s1 onto main", "label s1 needs-human"]);
    assert!(item(&r, "s1").reason.contains("migrations/0042.sql"));
}

#[tokio::test]
async fn resolves_go_one_at_a_time_per_repository_and_questions_are_labelled_once() {
    let fake = Fake::new().main(vec![main_green()]).pr("s2", vec![conflicted(2)]);
    let mut sessions = vec![session("s1", 1), session("s2", 2)];
    status(&mut sessions, "s1", SessionStatus::WaitingForAnswer);
    let mut mem = BTreeMap::from([(
        "acme/web".to_string(),
        RepoMemory {
            resolving: BTreeMap::from([(
                "https://github.com/acme/web/pull/1".to_string(),
                Resolving {
                    colony: "s1".into(),
                    attempts: 1,
                    ..Resolving::default()
                },
            )]),
            ..RepoMemory::default()
        },
    )]);
    let r = run(&fake, &cfg_resolve(), &sessions, &mut mem, false).await;
    assert_eq!(fake.writes(), vec!["label s1 needs-human"]);
    assert!(item(&r, "s1").reason.contains("asked a question"));
    assert!(item(&r, "s2").reason.contains("one at a time"), "{}", item(&r, "s2").reason);
    fake.log.borrow_mut().clear();
    run(&fake, &cfg_resolve(), &sessions, &mut mem, false).await;
    assert!(fake.writes().is_empty(), "labelled once: {:?}", fake.writes());

    // A resolve that stopped without publishing is reset: merge aborted, back in the train.
    status(&mut sessions, "s1", SessionStatus::Stopped);
    fake.log.borrow_mut().clear();
    let r = run(&fake, &cfg_resolve(), &sessions, &mut mem, false).await;
    assert!(fake.writes().contains(&"reset s1".to_string()), "{:?}", fake.writes());
    assert!(item(&r, "s1").reason.contains("back in the train"));
}

#[tokio::test]
async fn a_dry_run_only_says_it_would_resolve_and_the_switch_keeps_the_old_path() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![conflicted(1)]);
    let r = run(&fake, &cfg_resolve(), &[session("s1", 1)], &mut BTreeMap::new(), true).await;
    assert!(fake.writes().is_empty());
    assert!(item(&r, "s1").reason.contains("would merge main in"));
    // Off (the default): the mechanical rebase, as before.
    let off = Fake::new().main(vec![main_green()]).pr("s1", vec![conflicted(1)]);
    run(&off, &cfg(), &[session("s1", 1)], &mut BTreeMap::new(), false).await;
    assert_eq!(off.writes(), vec!["rebase s1"]);
}
