//! Issue #1075: the loop merges only a quiet head, only on checks of that exact head, pinned to it,
//! and raises a branch that got commits after the merged head.

use super::*;

fn pushed(fake: &Fake, sha: &str, ci: merge_head::HeadCi, at: DateTime<Utc>) {
    fake.heads
        .borrow_mut()
        .insert(sha.into(), HeadReading { ci, pushed_at: Some(at) });
}

#[tokio::test]
async fn a_push_inside_the_quiet_period_delays_the_merge() {
    // Pushed three minutes ago: the run waits the remaining seven, inside its CI wait, then merges.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    pushed(&fake, "head1", merge_head::HeadCi::Green, t0() - ChronoDuration::minutes(3));
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut BTreeMap::new(), false).await;
    assert_eq!(item(&r, "s1").action, Action::Merged);
    let at = fake.merged_at.borrow()[0];
    assert!(
        at >= t0() + ChronoDuration::minutes(7),
        "merged at {at}, before the head was quiet"
    );

    // Pushed just now, with a five-minute CI wait: the run does not sit out ten, the next one merges.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    pushed(&fake, "head1", merge_head::HeadCi::Green, t0());
    let mut settings = cfg();
    settings.ci_wait_minutes = 5;
    let mut memory = BTreeMap::new();
    let r = run(&fake, &settings, &[session("s1", 1)], &mut memory, false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Waiting);
    assert!(i.reason.contains("quiet for 10 min"), "{}", i.reason);
    fake.now.set(Some(t0() + ChronoDuration::minutes(11)));
    let r = run(&fake, &settings, &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(item(&r, "s1").action, Action::Merged);
    assert_eq!(fake.writes(), vec!["merge s1 Change 1 (#1)".to_string()]);
}

#[tokio::test]
async fn a_head_mismatch_at_merge_time_is_refused_and_retried_later() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    fake.merges.borrow_mut().push_back(Err(
        "gh: Head branch was modified. Review and try the merge again. (HTTP 409)".into(),
    ));
    let mut memory = BTreeMap::new();
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(
        fake.writes(),
        vec!["refused s1 sha=head1".to_string()],
        "pinned to the head it read"
    );
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Waiting);
    assert!(i.reason.contains("head moved"), "{}", i.reason);
    assert_eq!(memory["acme/web"].last_train_merge, None);
    // The next run finds the head settled and merges it.
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut memory, false).await;
    assert_eq!(item(&r, "s1").action, Action::Merged);
}

#[tokio::test]
async fn a_green_run_on_an_older_head_does_not_count() {
    // The pull request's rollup reads green, but nothing has run on its current head yet.
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    pushed(&fake, "head1", merge_head::HeadCi::NotRun, t0() - ChronoDuration::hours(1));
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut BTreeMap::new(), false).await;
    assert!(fake.writes().is_empty(), "{:?}", fake.writes());
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Waiting);
    assert!(i.reason.contains("earlier head does not count"), "{}", i.reason);
}

#[tokio::test]
async fn commits_after_the_merged_head_are_raised_and_keep_the_branch() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    fake.tips
        .borrow_mut()
        .insert("colonizer/issue-1".into(), "head1-later".into());
    let mut memory = BTreeMap::new();
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut memory, false).await;
    let i = item(&r, "s1");
    assert_eq!(i.action, Action::Merged);
    assert!(i.reason.contains("branch is kept"), "{}", i.reason);
    assert!(
        !fake.log.borrow().iter().any(|l| l.starts_with("delete-branch")),
        "the later commits keep their branch"
    );
    let unmerged = &r.repos[0].unmerged;
    assert_eq!(unmerged.len(), 1);
    assert_eq!(
        (
            unmerged[0].pr_url.as_str(),
            unmerged[0].merged_head.as_str(),
            unmerged[0].tip.as_str()
        ),
        ("https://github.com/acme/web/pull/1", "head1", "head1-later")
    );
    assert!(
        r.lines.iter().any(|l| l.contains("#1 needs attention: commits not merged")),
        "{:?}",
        r.lines
    );
}

#[tokio::test]
async fn the_report_names_the_merged_head() {
    let fake = Fake::new().main(vec![main_green()]).pr("s1", vec![green(1)]);
    let mut memory = BTreeMap::new();
    let r = run(&fake, &cfg(), &[session("s1", 1)], &mut memory, false).await;
    assert!(
        item(&r, "s1").reason.contains("merged head head1"),
        "{}",
        item(&r, "s1").reason
    );
    assert!(r.lines.iter().any(|l| l.contains("merged head head1")), "{:?}", r.lines);
    assert_eq!(
        memory["acme/web"].last_train_merge.as_ref().and_then(|m| m.head.as_deref()),
        Some("head1")
    );
    assert!(r.repos[0].unmerged.is_empty());
    assert!(fake.log.borrow().contains(&"delete-branch colonizer/issue-1".to_string()));
}
