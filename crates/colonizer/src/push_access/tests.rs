use super::*;
use crate::sessions::SessionStatus;
use crate::sessions::tests::colony;

/// A fixed clock, so the TTL is a fact about the test rather than about how fast it ran.
fn at(minutes: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-01T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + chrono::Duration::minutes(minutes)
}

// ---------------------------------------------------------------------------------------------
// The refusal sentence.
// ---------------------------------------------------------------------------------------------

/// The message is the operator's only instruction here, so it is pinned verbatim: which account,
/// which repository, and the two ways out.
#[test]
fn the_refusal_names_the_account_the_repository_and_the_way_out() {
    assert_eq!(
        refusal("EarthCollectivExchange", "Colonizer-dev/harness"),
        "this mothership signs in to GitHub as EarthCollectivExchange, which can't push to \
         Colonizer-dev/harness; launch it on a mothership that can, or give EarthCollectivExchange \
         write access"
    );
}

// ---------------------------------------------------------------------------------------------
// The cache.
// ---------------------------------------------------------------------------------------------

/// A decided answer is remembered for the TTL and re-asked once it has passed: the queue launches
/// a run of colonies on one repository, and only the first of them should spend a GitHub call.
#[test]
fn a_decided_answer_is_remembered_for_its_ttl_and_asked_again_after_it() {
    let cache = AccessCache::new(600);
    let who = "sha256=aaa";
    let now = at(0);

    assert_eq!(cache.get(who, "acme/app", now), None, "nothing is known yet");
    cache.put(who, "acme/app", now, &Verdict::CanPush);
    assert_eq!(cache.get(who, "acme/app", at(1)), Some(Verdict::CanPush), "one minute later");
    assert_eq!(
        cache.get(who, "acme/app", at(9)),
        Some(Verdict::CanPush),
        "just inside the TTL"
    );
    assert_eq!(
        cache.get(who, "acme/app", at(10)),
        None,
        "the TTL has passed, so GitHub is asked again"
    );
}

/// A GitHub that could not be asked is not an answer, and must not be remembered as one: a launch
/// must never be blocked by the blip that preceded it.
#[test]
fn an_unknown_is_never_remembered() {
    let cache = AccessCache::new(600);
    let now = at(0);
    // A GitHub that could not be asked leaves nothing behind: `put` refuses `Unknown`, so the next
    // launch asks again rather than being told the blip's answer for ten minutes.
    cache.put("sha256=aaa", "acme/app", now, &Verdict::Unknown);
    assert_eq!(cache.get("sha256=aaa", "acme/app", now), None);
    // A decided answer for the same key still lands, which is what makes the refusal above a rule
    // about `Unknown` and not a broken cache.
    cache.put("sha256=aaa", "acme/app", now, &Verdict::CanPush);
    assert_eq!(cache.get("sha256=aaa", "acme/app", now), Some(Verdict::CanPush));
}

/// One repository, two accounts: the answer belongs to the identity that gave it. This is what
/// keeps a mothership that reconnects as another account from being told the old account's
/// verdict.
#[test]
fn two_identities_get_independent_answers_for_one_repository() {
    let cache = AccessCache::new(600);
    let now = at(0);
    cache.put("sha256=aaa", "acme/app", now, &Verdict::CanPush);
    cache.put(
        "sha256=bbb",
        "acme/app",
        now,
        &Verdict::CannotPush {
            login: "someone-else".into(),
        },
    );
    assert_eq!(cache.get("sha256=aaa", "acme/app", now), Some(Verdict::CanPush));
    assert_eq!(
        cache.get("sha256=bbb", "acme/app", now),
        Some(Verdict::CannotPush {
            login: "someone-else".into()
        }),
        "the other account keeps its own answer for the same repository"
    );
    assert_eq!(
        cache.get("sha256=ccc", "acme/app", now),
        None,
        "a third one has asked nothing"
    );
}

/// GitHub repository names are case-insensitive, and a launch may name one either way.
#[test]
fn cache_keys_are_case_insensitive() {
    let cache = AccessCache::new(600);
    let now = at(0);
    cache.put("SHA256=AAA", "Acme/App", now, &Verdict::CannotPush { login: "octo".into() });
    assert_eq!(
        cache.get("sha256=aaa", "acme/app", now),
        cache.get("SHA256=AAA", "Acme/App", now),
        "one question, asked two ways"
    );
    assert!(cache.get("sha256=aaa", "acme/app", now).is_some());
}

/// The live cache really is ten minutes, which is what the launch path is budgeted against.
#[test]
fn the_live_cache_answers_within_its_ten_minute_ttl() {
    let now = at(0);
    CACHE.put("sha256=ttl-probe", "acme/app", now, &Verdict::CanPush);
    assert_eq!(CACHE.get("sha256=ttl-probe", "acme/app", at(9)), Some(Verdict::CanPush));
    assert_eq!(CACHE.get("sha256=ttl-probe", "acme/app", at(11)), None);
    assert_eq!(PUSH_ACCESS_TTL_SECS, 600);
}

// ---------------------------------------------------------------------------------------------
// The classifier, on what git and gh really print.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_push_refused_for_want_of_access_is_recognised() {
    for text in [
        // What issue #1134 is: hours of work, one push, this message.
        "git push failed (exit status: 128): remote: Permission to Colonizer-dev/harness.git denied to \
         EarthCollectivExchange.\nfatal: unable to access 'https://github.com/Colonizer-dev/harness.git/': \
         The requested URL returned error: 403",
        // The same refusal without the URL line.
        "remote: Permission to Colonizer-dev/harness.git denied to EarthCollectivExchange.",
        // A dead credential reaches the push as well, and is the same dead end.
        "HTTP 401: Bad credentials",
        "gh: Bad credentials (HTTP 401)",
    ] {
        assert!(refuses_push(text), "{text}");
    }
}

/// The three failures that must NOT be read as a permission problem (issue #1206): a conflict and
/// a rejected secret are `PublishHold`s the colony fixes itself, and a missing branch is a bug in
/// our own push, not GitHub's answer.
#[test]
fn a_conflict_a_secret_and_a_missing_branch_are_not_permission_problems() {
    for text in [
        "! [rejected]        colonizer/issue-1134 -> colonizer/issue-1134 (non-fast-forward)\n\
         error: failed to push some refs to 'https://github.com/Colonizer-dev/harness.git'\n\
         hint: Updates were rejected because the remote contains work that you do not have locally.",
        "GH013: Push failed on Colonizer-dev/feature due to secret detected. \
         Please remove secret from your code.",
        "fatal: unable to push to 'https://github.com/Colonizer-dev/harness.git': \
         The requested branch does not exist",
    ] {
        assert!(!refuses_push(text), "{text}");
    }
}

// ---------------------------------------------------------------------------------------------
// The publish side: a 403 parks the colony instead of failing it.
// ---------------------------------------------------------------------------------------------

/// A publish refused for want of write access must leave the colony recoverable: `Failed` answers
/// 409 to both resume and publish, `Parked` answers to neither. So the work, which is already in
/// the worktree and the branch, is one Retry away from landing once the account has access.
#[tokio::test]
async fn a_push_refused_for_want_of_access_parks_the_colony_and_keeps_it_recoverable() {
    let root = std::env::temp_dir().join(format!("colonizer-push-access-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    let mut s = colony("acme", SessionStatus::Publishing);
    s.id = "refused".into();
    s.git_admin_dir = Some("git".into());
    *app.sessions.write().await = vec![s];

    crate::publish::park_without_push_access(
        &app,
        "refused",
        "remote: Permission to acme/app.git denied to octo.\nfatal: unable to access \
         'https://github.com/acme/app.git/': The requested URL returned error: 403",
    )
    .await;

    let after = app.session("refused").await.expect("the colony is still there");
    assert_eq!(after.status, SessionStatus::Parked, "not Failed, which is terminal");
    let parked = after.parked.as_ref().expect("a park record");
    assert_eq!(parked.reason, crate::publish::NO_PUSH_ACCESS_REASON);
    assert!(!parked.vm_kept, "the microVM was confirmed removed before the push ran");
    assert_eq!(parked.resets_at, None, "nothing here is a wait with an end");
    assert_eq!(after.publish_stage, None, "the publish is over");
    assert!(
        crate::publish::can_publish(after.status, after.cleaned_up, true),
        "Retry is offered: the worktree is on disk and the status is publishable"
    );
    assert!(
        crate::lifecycle::can_resume(after.status, after.cleaned_up, true),
        "the colony is resumable too"
    );
    let error = after.error.expect("the operator is told what happened");
    assert!(error.contains("Permission to acme/app.git denied to octo"), "{error}");
    assert!(
        error.contains("your work is saved") && error.contains("write access"),
        "and what to do about it: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}
