use super::*;
use crate::github::Mergeability::*;
use crate::orgs::AutoMerge::*;

fn run(name: &str, outcome: Outcome) -> Check {
    Check {
        name: name.into(),
        outcome,
        duration_secs: Some(240),
        steps: Some(9),
        annotations: Vec::new(),
        url: Some("https://github.com/acme/api/actions/runs/77/job/4242".into()),
    }
}

/// A job that failed before it started anything.
fn dead(name: &str) -> Check {
    Check {
        duration_secs: Some(3),
        steps: Some(0),
        ..run(name, Outcome::Fail)
    }
}

fn facts(checks: Vec<Check>) -> Facts {
    Facts {
        url: "https://github.com/acme/api/pull/7".into(),
        title: "Fix it".into(),
        open: true,
        draft: false,
        cross_repository: false,
        labels: Vec::new(),
        mergeability: Clean,
        merge_state: "CLEAN".into(),
        review: None,
        head: "abc123".into(),
        adds_fragment: false,
        checks,
    }
}

fn ctx(mode: AutoMerge) -> Ctx {
    Ctx {
        mode,
        colony_idle: true,
        ..Ctx::default()
    }
}

fn green() -> Vec<Check> {
    vec![run("build", Outcome::Pass), run("test", Outcome::Pass)]
}

/// The kind of an action, so a table can name what it expects without spelling out every payload.
fn kind(a: &Action) -> &'static str {
    match a {
        Action::Skip(_) => "skip",
        Action::Wait(_) => "wait",
        Action::Merge => "merge",
        Action::UpdateBranch => "update_branch",
        Action::ResumeRebase => "resume_rebase",
        Action::ResumeFix(_) => "resume_fix",
        Action::CiBlocked(_) => "ci_blocked",
        Action::NeedsAttention(_) => "needs_attention",
    }
}

#[test]
fn the_decision_table() {
    type Edit = fn(&mut Facts, &mut Ctx);
    let table: &[(&str, Edit, &str)] = &[
        ("green and clean merges", |_, _| {}, "merge"),
        ("HAS_HOOKS merges too", |f, _| f.merge_state = "HAS_HOOKS".into(), "merge"),
        ("off never touches it", |_, c| c.mode = Off, "skip"),
        ("a closed pull request is skipped", |f, _| f.open = false, "skip"),
        ("a draft waits", |f, _| f.draft = true, "wait"),
        ("a fork waits", |f, _| f.cross_repository = true, "wait"),
        ("the hold label waits", |f, _| f.labels = vec!["Hold".into()], "wait"),
        ("do-not-merge waits", |f, _| f.labels = vec!["do-not-merge".into()], "wait"),
        (
            "do not merge (spaces) waits",
            |f, _| f.labels = vec!["Do Not Merge".into()],
            "wait",
        ),
        ("needs-human waits", |f, _| f.labels = vec!["needs-human".into()], "wait"),
        (
            "an unrelated label does not hold",
            |f, _| f.labels = vec!["bug".into()],
            "merge",
        ),
        (
            "a fragment waits while a release is open",
            |f, c| {
                f.adds_fragment = true;
                c.release_open = true;
            },
            "wait",
        ),
        (
            "a fragment merges when no release is open",
            |f, _| f.adds_fragment = true,
            "merge",
        ),
        (
            "a pull request without a fragment merges under a release",
            |_, c| c.release_open = true,
            "merge",
        ),
        ("a colony marked needing a person waits", |_, c| c.needs_human = true, "wait"),
        ("a colony still working waits", |_, c| c.colony_idle = false, "wait"),
        (
            "running checks wait",
            |f, _| f.checks.push(run("lint", Outcome::Pending)),
            "wait",
        ),
        ("no checks yet waits", |f, _| f.checks.clear(), "wait"),
        (
            "changes requested waits",
            |f, _| f.review = Some("CHANGES_REQUESTED".into()),
            "wait",
        ),
        (
            "BLOCKED (a required check or review) waits",
            |f, _| f.merge_state = "BLOCKED".into(),
            "wait",
        ),
        (
            "UNSTABLE never merges unattended",
            |f, _| f.merge_state = "UNSTABLE".into(),
            "wait",
        ),
        ("unknown mergeability waits", |f, _| f.mergeability = Unknown, "wait"),
        (
            "a real failing check resumes the colony",
            |f, _| f.checks.push(run("test", Outcome::Fail)),
            "resume_fix",
        ),
        (
            "a failure that never ran is ci_blocked",
            |f, _| f.checks = vec![dead("build"), dead("test")],
            "ci_blocked",
        ),
        (
            "behind waits under green",
            |f, _| {
                f.mergeability = Behind;
                f.merge_state = "BEHIND".into();
            },
            "wait",
        ),
        (
            "conflicting waits under green",
            |f, _| {
                f.mergeability = Conflicted;
                f.merge_state = "DIRTY".into();
            },
            "wait",
        ),
        (
            "behind tries update-branch under green+rebase",
            |f, c| {
                f.mergeability = Behind;
                c.mode = GreenRebase;
            },
            "update_branch",
        ),
        (
            "conflicting tries update-branch first under green+rebase",
            |f, c| {
                f.mergeability = Conflicted;
                c.mode = GreenRebase;
            },
            "update_branch",
        ),
        (
            "update-branch already tried and the watcher flagged it: resume the colony",
            |f, c| {
                f.mergeability = Conflicted;
                c.mode = GreenRebase;
                c.update_tried_head = Some("abc123".into());
                c.rebase_flagged = true;
            },
            "resume_rebase",
        ),
        (
            "update-branch tried but the watcher has not flagged it: wait for the watcher",
            |f, c| {
                f.mergeability = Conflicted;
                c.mode = GreenRebase;
                c.update_tried_head = Some("abc123".into());
            },
            "wait",
        ),
        (
            "a new head earns another update-branch",
            |f, c| {
                f.mergeability = Behind;
                c.mode = GreenRebase;
                c.update_tried_head = Some("older".into());
            },
            "update_branch",
        ),
        (
            "a conflict beats a failing check",
            |f, c| {
                f.mergeability = Conflicted;
                f.checks.push(run("test", Outcome::Fail));
                c.mode = GreenRebase;
            },
            "update_branch",
        ),
        (
            "a mode of green+rebase still merges a clean green pull request",
            |_, c| c.mode = GreenRebase,
            "merge",
        ),
    ];
    for (name, edit, want) in table {
        let (mut f, mut c) = (facts(green()), ctx(Green));
        edit(&mut f, &mut c);
        let got = decide(&f, &c);
        assert_eq!(kind(&got), *want, "{name}: got {got:?}");
    }
}

#[test]
fn a_merge_never_happens_unless_github_says_clean_and_every_check_passed() {
    for state in ["BLOCKED", "BEHIND", "DIRTY", "DRAFT", "UNSTABLE", "UNKNOWN", ""] {
        let mut f = facts(green());
        f.merge_state = state.into();
        assert_ne!(decide(&f, &ctx(GreenRebase)), Action::Merge, "{state}");
    }
    for bad in [Outcome::Pending, Outcome::Fail] {
        let mut f = facts(green());
        f.checks.push(run("required", bad));
        assert_ne!(decide(&f, &ctx(GreenRebase)), Action::Merge, "{bad:?}");
    }
}

#[test]
fn fixing_a_failing_check_is_bounded_to_two_rounds() {
    let mut f = facts(green());
    f.checks.push(run("test", Outcome::Fail));
    let mut c = ctx(Green);
    for round in 0..MAX_ROUNDS {
        c.fix_rounds = round;
        c.fixed_head = Some(format!("older-{round}"));
        assert_eq!(kind(&decide(&f, &c)), "resume_fix", "round {}", round + 1);
    }
    c.fix_rounds = MAX_ROUNDS;
    match decide(&f, &c) {
        Action::NeedsAttention(why) => assert!(why.contains("rounds"), "{why}"),
        other => panic!("the third round is a person's: {other:?}"),
    }
}

#[test]
fn a_colony_that_pushed_nothing_is_not_resumed_again_for_the_same_head() {
    let mut f = facts(green());
    f.checks.push(run("test", Outcome::Fail));
    let mut c = ctx(Green);
    c.fix_rounds = 1;
    c.fixed_head = Some(f.head.clone());
    assert_eq!(kind(&decide(&f, &c)), "needs_attention");
    c.fixed_head = Some("the-head-before-the-fix".into());
    assert_eq!(kind(&decide(&f, &c)), "resume_fix", "a new head is a new chance");
}

#[test]
fn rebasing_a_colony_is_bounded_to_two_rounds() {
    let mut f = facts(green());
    f.mergeability = Conflicted;
    let mut c = ctx(GreenRebase);
    c.update_tried_head = Some(f.head.clone());
    c.rebase_flagged = true;
    for round in 0..MAX_ROUNDS {
        c.rebase_rounds = round;
        assert_eq!(decide(&f, &c), Action::ResumeRebase);
    }
    c.rebase_rounds = MAX_ROUNDS;
    assert_eq!(kind(&decide(&f, &c)), "needs_attention");
}

#[test]
fn the_failing_checks_ride_along_with_the_fix_action() {
    let mut f = facts(green());
    f.checks.push(run("e2e", Outcome::Fail));
    match decide(&f, &ctx(Green)) {
        Action::ResumeFix(failed) => assert_eq!(failed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["e2e"]),
        other => panic!("{other:?}"),
    }
}

// --- ci_blocked detection ----------------------------------------------------------------------

#[test]
fn every_job_failing_instantly_with_no_steps_is_ci_blocked() {
    let verdict = judge_checks(&[dead("build"), dead("test"), run("lint", Outcome::Pass)]);
    match verdict {
        ChecksVerdict::CiBlocked(why) => assert!(why.contains("under 10s") && why.contains("build"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn one_real_failure_among_instant_ones_is_a_real_failure() {
    match judge_checks(&[dead("build"), run("test", Outcome::Fail)]) {
        ChecksVerdict::Failed(failed) => assert_eq!(failed.len(), 2),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_fast_failure_that_ran_steps_is_not_blocked() {
    let quick = Check {
        duration_secs: Some(4),
        steps: Some(3),
        ..run("lint", Outcome::Fail)
    };
    assert!(matches!(judge_checks(&[quick]), ChecksVerdict::Failed(_)));
}

#[test]
fn a_slow_failure_with_no_steps_is_not_blocked() {
    let slow = Check {
        duration_secs: Some(10),
        steps: Some(0),
        ..run("lint", Outcome::Fail)
    };
    assert!(
        matches!(judge_checks(&[slow]), ChecksVerdict::Failed(_)),
        "10s is not under 10s"
    );
}

#[test]
fn a_status_and_an_unknown_step_count_are_never_read_as_blocked() {
    let status = Check {
        duration_secs: None,
        steps: None,
        ..run("ci/circle", Outcome::Fail)
    };
    assert!(matches!(judge_checks(&[status]), ChecksVerdict::Failed(_)));
    let unknown = Check {
        steps: None,
        ..dead("build")
    };
    assert!(matches!(judge_checks(&[unknown]), ChecksVerdict::Failed(_)));
}

#[test]
fn a_billing_annotation_is_ci_blocked_whatever_the_timing() {
    for note in [
        "The job was not started because recent account payments have failed or your spending limit needs to be increased.",
        "Check your Billing & plans settings",
        "Spending limit reached",
    ] {
        let mut c = run("build", Outcome::Fail);
        c.annotations = vec![note.into()];
        assert!(matches!(judge_checks(&[c]), ChecksVerdict::CiBlocked(_)), "{note}");
    }
    let mut c = run("build", Outcome::Fail);
    c.annotations = vec!["error: expected `;`".into()];
    assert!(matches!(judge_checks(&[c]), ChecksVerdict::Failed(_)));
}

#[test]
fn checks_sum_up_to_one_verdict() {
    assert_eq!(judge_checks(&[]), ChecksVerdict::Nothing);
    assert_eq!(judge_checks(&green()), ChecksVerdict::Green);
    let mut running = green();
    running.push(run("e2e", Outcome::Pending));
    assert_eq!(judge_checks(&running), ChecksVerdict::Pending);
    let mut failed_while_running = running.clone();
    failed_while_running.push(run("test2", Outcome::Fail));
    assert!(
        matches!(judge_checks(&failed_while_running), ChecksVerdict::Failed(_)),
        "a failure does not wait"
    );
}

// --- Reading GitHub ---------------------------------------------------------------------------

fn node() -> Value {
    json!({
        "number": 7, "url": "https://github.com/acme/api/pull/7", "title": "Fix it", "state": "OPEN",
        "isDraft": false, "isCrossRepository": false, "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN",
        "reviewDecision": null, "headRefOid": "abc123",
        "labels": {"nodes": [{"name": "bug"}]},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
            {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "FAILURE",
             "startedAt": "2026-10-07T10:00:00Z", "completedAt": "2026-10-07T10:00:03Z",
             "detailsUrl": "https://github.com/acme/api/actions/runs/77/job/4242",
             "steps": {"totalCount": 0},
             "annotations": {"nodes": [{"title": "", "message": "payments have failed"}]}},
            {"__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "conclusion": null,
             "startedAt": "2026-10-07T10:00:00Z", "completedAt": null, "detailsUrl": null,
             "steps": {"totalCount": 4}, "annotations": {"nodes": []}},
            {"__typename": "StatusContext", "context": "ci/x", "state": "SUCCESS", "targetUrl": null}
        ]}}}}]}
    })
}

#[test]
fn a_graphql_node_reads_into_facts() {
    let f = parse_pr(&node()).expect("facts");
    assert!(f.open && !f.draft && !f.cross_repository);
    assert_eq!(
        (f.mergeability, f.merge_state.as_str(), f.head.as_str()),
        (Clean, "CLEAN", "abc123")
    );
    assert_eq!(f.labels, ["bug"]);
    assert_eq!(f.checks.len(), 3);
    let build = &f.checks[0];
    assert_eq!(
        (build.outcome, build.duration_secs, build.steps),
        (Outcome::Fail, Some(3), Some(0))
    );
    assert!(billing_annotation(build).is_some());
    assert!(never_ran(build));
    assert_eq!(f.checks[1].outcome, Outcome::Pending);
    assert_eq!(f.checks[2].outcome, Outcome::Pass);
    assert!(matches!(judge_checks(&f.checks), ChecksVerdict::CiBlocked(_)));
}

#[test]
fn fragments_and_release_titles_are_recognised() {
    assert!(is_fragment_path("changelog.d/1193.added.md"));
    assert!(!is_fragment_path("changelog.d/README.md"));
    assert!(!is_fragment_path("changelog.d/.gitkeep"));
    assert!(!is_fragment_path("docs/changelog.d/1.fixed.md"));
    assert!(!is_fragment_path("CHANGELOG.md"));
    assert!(is_release_title("release: v0.2.12"));
    assert!(is_release_title("release: v0.2.12 (#1200)"));
    assert!(!is_release_title("release: vNext"));
    assert!(!is_release_title("fix: release: v0.2.12"));
    let mut n = node();
    assert!(!parse_pr(&n).unwrap().adds_fragment);
    n["files"] = json!({"nodes": [{"path": "src/a.rs"}, {"path": "changelog.d/9.fixed.md"}]});
    assert!(parse_pr(&n).unwrap().adds_fragment);
}

#[test]
fn a_missing_pull_request_or_a_fork_field_fails_closed() {
    assert!(parse_pr(&Value::Null).is_none());
    let mut n = node();
    n.as_object_mut().unwrap().remove("isCrossRepository");
    assert!(
        parse_pr(&n).unwrap().cross_repository,
        "a field GitHub left out reads as a fork"
    );
    let mut n = node();
    n["mergeable"] = Value::Null;
    n["mergeStateStatus"] = Value::Null;
    let f = parse_pr(&n).unwrap();
    assert_eq!((f.mergeability, f.merge_state.as_str()), (Unknown, "UNKNOWN"));
}

#[test]
fn pull_request_urls_parse_and_the_query_has_an_alias_each() {
    assert_eq!(
        parse_pr_url("https://github.com/acme/api/pull/7"),
        Some(("acme/api".into(), 7))
    );
    assert_eq!(
        parse_pr_url("https://github.com/acme/api/pull/7/"),
        Some(("acme/api".into(), 7))
    );
    for bad in [
        "https://github.com/acme/api/issues/7",
        "https://github.com/acme/api/pull/x",
        "https://github.com/acme/api/pull/7/files",
        "https://evil.example/acme/api/pull/7",
        "https://github.com/ac me/api/pull/7",
        "https://github.com/acme\"/api/pull/7",
    ] {
        assert_eq!(parse_pr_url(bad), None, "{bad}");
    }
    let q = build_query(&[("acme/api".into(), 7), ("acme/web".into(), 9)]);
    assert!(
        q.contains("p0:repository(owner:\"acme\",name:\"api\"){pullRequest(number:7)"),
        "{q}"
    );
    assert!(
        q.contains("p1:repository(owner:\"acme\",name:\"web\"){pullRequest(number:9)"),
        "{q}"
    );
    assert_eq!(q.matches("query{").count(), 1, "one query for the org");
}

// --- Acting -----------------------------------------------------------------------------------

#[test]
fn the_merge_is_pinned_to_the_head_that_was_read() {
    use crate::orgs::MergeMethod::*;
    assert_eq!(
        merge_args("https://github.com/acme/api/pull/7", "abc", Squash, false, false),
        [
            "pr",
            "merge",
            "https://github.com/acme/api/pull/7",
            "--squash",
            "--match-head-commit",
            "abc"
        ]
    );
    let all = merge_args("u", "abc", Rebase, true, true);
    assert!(all.contains(&"--rebase".into()) && all.contains(&"--delete-branch".into()) && all.contains(&"--auto".into()));
    assert!(merge_args("u", "abc", Merge, false, false).contains(&"--merge".into()));
}

#[test]
fn a_merge_queue_refusal_asks_for_auto_merge_and_a_conflict_is_a_conflict() {
    assert!(wants_auto_merge("Pull request is in a merge queue; use --auto"));
    assert!(wants_auto_merge("auto-merge is required"));
    assert!(!wants_auto_merge("Pull request is not mergeable"));
    assert!(is_conflict("HTTP 422: merge conflict between base and head"));
    assert!(!is_conflict("HTTP 403: Resource not accessible"));
}

#[test]
fn a_failing_job_is_found_by_its_url_and_its_log_is_clipped_to_the_tail() {
    assert_eq!(job_id("https://github.com/acme/api/actions/runs/77/job/4242"), Some(4242));
    assert_eq!(
        job_id("https://github.com/acme/api/actions/runs/77/job/4242?pr=7"),
        Some(4242)
    );
    assert_eq!(job_id("https://example.com/status"), None);
    assert_eq!(log_tail("abcdef\n", 3), "def");
    assert_eq!(log_tail("ab", 10), "ab");
    let note = fix_note(
        "https://github.com/acme/api/pull/7",
        &[("test".into(), "FAILED x".into()), ("lint".into(), String::new())],
        1,
    );
    assert!(
        note.contains("round 1 of 2") && note.contains("### Failing job: test") && note.contains("FAILED x"),
        "{note}"
    );
    assert!(note.contains("no log could be read"), "{note}");
    assert!(rebase_note("u", "main", 2).contains("round 2 of 2"));
}

#[test]
fn hold_labels_match_whatever_the_case() {
    for l in ["hold", "HOLD", " Do-Not-Merge ", "do not merge", "needs-human"] {
        assert!(is_hold_label(l), "{l}");
    }
    for l in ["holding", "merge", "bug"] {
        assert!(!is_hold_label(l), "{l}");
    }
}

#[test]
fn merge_now_refuses_what_github_does_not_call_mergeable() {
    let mut f = facts(green());
    assert_eq!(manual_refusal(&f), None);
    f.merge_state = "UNSTABLE".into();
    assert_eq!(manual_refusal(&f), None, "a person may merge past a non-required failure");
    for (state, word) in [
        ("BLOCKED", "protection"),
        ("DIRTY", "conflicts"),
        ("BEHIND", "behind"),
        ("UNKNOWN", "UNKNOWN"),
    ] {
        f.merge_state = state.into();
        assert!(manual_refusal(&f).unwrap().contains(word), "{state}");
    }
    let mut f = facts(green());
    f.draft = true;
    assert!(manual_refusal(&f).is_some());
    let mut f = facts(green());
    f.cross_repository = true;
    assert!(manual_refusal(&f).is_some());
}

#[test]
fn the_banner_names_the_org() {
    assert!(banner_message("Kontinuum-ai").contains("GitHub Actions is blocked for Kontinuum-ai"));
}

#[tokio::test]
async fn the_memory_round_trips_and_a_missing_file_is_empty() {
    let dir = std::env::temp_dir().join(format!("colonizer-steward-{}", crate::util::short_id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(load(&dir).await.prs.is_empty());
    update(&dir, |m| {
        m.rounds.insert(
            "c1".into(),
            Rounds {
                fix: 2,
                rebase: 1,
                fix_head: Some("h".into()),
            },
        );
        m.ci_blocked.insert(
            "acme".into(),
            CiBlock {
                since: Utc::now(),
                reason: "r".into(),
                prs: vec!["u".into()],
            },
        );
    })
    .await
    .unwrap();
    let back = load(&dir).await;
    assert_eq!((back.rounds["c1"].fix, back.rounds["c1"].rebase), (2, 1));
    assert_eq!(back.ci_blocked["acme"].prs, ["u"]);
    let _ = std::fs::remove_dir_all(&dir);
}
