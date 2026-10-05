use super::*;
use crate::sessions::tests::*;
use tokio::sync::RwLock;

/// Nothing configured means nothing automatic; the publish module's defaults flow into a session
/// that did not choose, and a module-default automerge without autofix is nothing, while an
/// explicit automerge on a fix colony counts on its own — that explicit value IS the hunter's
/// decision, set at the fix colony's creation.
#[tokio::test]
async fn autofix_and_automerge_fall_back_to_the_module_and_count_explicit_choices() {
    let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
    let s = app.session("abc").await.unwrap();
    assert!(!autofix_enabled(&app, &s).await);
    assert!(!automerge_enabled(&app, &s).await);

    // The publish module's defaults flow down when the session did not choose.
    {
        let mut modules = app.modules.write().await;
        modules.publish.settings.insert("autofix".into(), json!(true));
        modules.publish.settings.insert("automerge".into(), json!(true));
    }
    assert!(autofix_enabled(&app, &s).await);
    assert!(automerge_enabled(&app, &s).await);

    // Automerge only counts when autofix is enabled: with autofix off, there are no fix
    // colonies, so the module-default automerge is a setting nobody can have meant.
    {
        let mut modules = app.modules.write().await;
        modules.publish.settings.remove("autofix");
    }
    assert!(!autofix_enabled(&app, &s).await);
    assert!(!automerge_enabled(&app, &s).await);

    // A fix colony carries its automerge explicitly from its hunter, so it counts even though
    // its autofix is `Some(false)` — the flag that stops a fix colony cascading.
    let mut fix = colony("acme", SessionStatus::Idle);
    fix.autofix = Some(false);
    fix.automerge = Some(true);
    assert!(automerge_enabled(&app, &fix).await);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn overlap_queueing_holds_the_oldest_live_same_repo_colony_with_a_worktree() {
    fn holder(id: &str, repo: &str, status: SessionStatus, ago_secs: i64) -> Session {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.repo = repo.into();
        s.git_admin_dir = Some("git".into());
        s.created_at = Utc::now() - chrono::Duration::seconds(ago_secs);
        s
    }
    let sessions = vec![
        holder("new", "acme/repo", SessionStatus::Running, 10),
        holder("old", "acme/repo", SessionStatus::Running, 100),
        holder("other-repo", "acme/other", SessionStatus::Running, 200),
        holder("published", "acme/repo", SessionStatus::PrOpened, 300),
        holder("queued", "acme/repo", SessionStatus::Queued, 400),
    ];
    assert_eq!(
        overlap_holder(&sessions, "acme/repo").as_deref(),
        Some("old"),
        "the oldest live holder wins"
    );
    assert_eq!(overlap_holder(&sessions, "acme/other").as_deref(), Some("other-repo"));
    assert_eq!(overlap_holder(&sessions, "acme/empty"), None, "no live worktree, no holder");
    // A live colony that never booted a worktree holds nothing back.
    let mut no_worktree = holder("wt-less", "acme/repo", SessionStatus::Running, 500);
    no_worktree.git_admin_dir = None;
    let mut only_quiet = vec![no_worktree];
    only_quiet.extend(sessions.into_iter().filter(|s| s.repo != "acme/repo" || !s.status.is_live()));
    assert_eq!(overlap_holder(&only_quiet, "acme/repo"), None);
}

#[test]
fn an_overlap_queued_colony_stays_queued_until_its_holder_finishes() {
    fn queued_behind(holder: &str) -> Session {
        let mut s = colony("acme", SessionStatus::Starting);
        s.id = "new".into();
        s.repo = "acme/repo".into();
        s.queued_behind = Some(holder.into());
        s
    }
    let mut live_holder = colony("acme", SessionStatus::Running);
    live_holder.id = "holder".into();
    live_holder.repo = "acme/repo".into();
    // Room and no parent, yet queued: the live holder keeps it waiting, and the pointer stays.
    let mut sessions = vec![live_holder.clone()];
    let (admitted, queued, _) = try_claim_session(
        &mut sessions,
        true,
        queued_behind("holder"),
        "acme/repo",
        None,
        false,
        false,
        false,
    )
    .expect("no issue race");
    assert!(
        queued && admitted.status == SessionStatus::Queued,
        "held behind the live colony"
    );
    assert_eq!(admitted.queued_behind.as_deref(), Some("holder"));
    // The holder published: the same pointer releases at once, pointing nowhere stale.
    live_holder.status = SessionStatus::PrOpened;
    let mut sessions = vec![live_holder];
    let (admitted, queued, _) = try_claim_session(
        &mut sessions,
        true,
        queued_behind("holder"),
        "acme/repo",
        None,
        false,
        false,
        false,
    )
    .expect("no issue race");
    assert!(
        !queued && admitted.status == SessionStatus::Starting,
        "released once the holder finished"
    );
    assert_eq!(admitted.queued_behind, None);
}

/// A `create` request with nothing but the repo and, where the test names one, whether to opt
/// into overlap-aware queueing.
fn overlap_request(repo: &str, serialize: Option<bool>) -> Json<NewSession> {
    Json(NewSession {
        repo: repo.into(),
        issue: None,
        title: String::new(),
        instructions: String::new(),
        autopilot: None,
        verify: None,
        autofix: None,
        automerge: None,
        allow_duplicate: false,
        allow_epic: false,
        queue_behind_holder: false,
        supply_chain: None,
        model_tier: None,
        model_override: None,
        subagent_model_override: None,
        claude_account: None,
        after: None,
        stack: false,
        origin: None,
        host: None,
        serialize,
        handoff: None,
    })
}

/// Issue #453's review: overlap-aware queueing must be opt-in. The same live, touched-file
/// holder is on the nest both times — only whether the request carries `serialize: true`
/// decides whether the newcomer queues behind it.
#[tokio::test]
async fn overlap_queueing_only_applies_when_the_request_opts_in_with_serialize() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);

    // A live same-repo colony with a real worktree and an untracked file: `touched_files`
    // reads that as something touched via `git status --porcelain`, without needing a remote
    // or any commits.
    let worktree = root.join("holder-worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&worktree)
        .status()
        .expect("git init");
    std::fs::write(worktree.join("touched.txt"), "x").unwrap();

    let mut holder = colony("acme", SessionStatus::Running);
    holder.id = "holder".into();
    holder.repo = "acme/app".into();
    holder.git_admin_dir = Some("git".into());
    holder.worktree = worktree.to_string_lossy().to_string();
    holder.created_at = Utc::now() - chrono::Duration::seconds(100);
    app.sessions.write().await.push(holder);

    // No `serialize` at all: the default stays off, so the newcomer never even scans for an
    // overlap and starts unheld.
    let created = create(State(app.clone()), None, overlap_request("acme/app", None))
        .await
        .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
    assert_eq!(
        created.queued_behind, None,
        "overlap queueing is opt-in; a plain launch never scans for it"
    );

    // `serialize: false` reads the same as absent.
    let created = create(State(app.clone()), None, overlap_request("acme/app", Some(false)))
        .await
        .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
    assert_eq!(created.queued_behind, None, "an explicit false is still off");

    // `serialize: true`: the same live, touched-file colony now holds the newcomer behind it.
    let created = create(State(app.clone()), None, overlap_request("acme/app", Some(true)))
        .await
        .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
    assert_eq!(
        created.queued_behind.as_deref(),
        Some("holder"),
        "serialize: true asks to queue behind a live colony with touched files"
    );
    assert_eq!(
        created.status,
        SessionStatus::Queued,
        "queued behind the live holder rather than starting alongside it"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// A colony on the issue, in a state where a second one duplicates its work.
fn on_issue(id: &str, issue: u64, status: SessionStatus) -> Session {
    let mut s = colony("acme", status);
    s.id = id.into();
    s.issue = Some(issue);
    s
}

#[test]
fn a_live_or_published_colony_holds_its_issue_against_a_second_one() {
    // FindsYou-Work/app #7 drew four colonies and #13 drew two, because nothing asked.
    for status in [
        SessionStatus::Queued,
        SessionStatus::Starting,
        SessionStatus::Running,
        SessionStatus::WaitingForAnswer,
        SessionStatus::Idle,
        SessionStatus::Publishing,
        SessionStatus::PrOpened,
    ] {
        let sessions = vec![on_issue("first", 7, status)];
        let held = issue_held_by(&sessions, "acme/repo", 7);
        assert_eq!(
            held.map(|s| s.id),
            Some("first".to_string()),
            "a colony that is {} still holds #7",
            status.as_str()
        );
    }
}

#[test]
fn a_finished_colony_leaves_its_issue_free_to_try_again() {
    for status in [
        SessionStatus::Merged,
        SessionStatus::Closed,
        SessionStatus::NoChanges,
        SessionStatus::Stopped,
        SessionStatus::Failed,
    ] {
        let sessions = vec![on_issue("first", 7, status)];
        assert!(
            issue_held_by(&sessions, "acme/repo", 7).is_none(),
            "{} is done with #7, so a retry is not a duplicate",
            status.as_str()
        );
    }
}

#[test]
fn the_hold_is_per_repository_and_per_issue() {
    let sessions = vec![
        on_issue("other-repo", 7, SessionStatus::Running),
        on_issue("other-issue", 8, SessionStatus::Running),
    ];
    let mut elsewhere = sessions.clone();
    elsewhere[0].repo = "acme/different".into();
    assert!(
        issue_held_by(&elsewhere, "acme/repo", 7).is_none(),
        "the same issue number in another repository is a different issue"
    );
    assert!(
        issue_held_by(&sessions, "acme/repo", 9).is_none(),
        "an untouched issue is free"
    );
    assert_eq!(
        issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
        Some("other-repo".to_string()),
        "the colony on this repo's #7 is the one that holds it"
    );
}

#[test]
fn a_stopped_or_failed_colony_frees_its_issue_for_an_explicit_retry() {
    // The sweep above covers every terminal state; stopped and failed get their own assertion
    // because they are the retries that matter — a run that died halfway, not one that shipped.
    for status in [SessionStatus::Stopped, SessionStatus::Failed] {
        let sessions = vec![on_issue("dead", 7, status)];
        assert!(
            issue_held_by(&sessions, "acme/repo", 7).is_none(),
            "{} is done with #7, so a retry is not a duplicate",
            status.as_str()
        );
        // And the atomic claim the handler admits with lets that retry through.
        let mut sessions = sessions;
        let mut retry = colony("acme", SessionStatus::Starting);
        retry.id = "retry".into();
        retry.issue = Some(7);
        let claimed = try_claim_session(&mut sessions, true, retry, "acme/repo", Some(7), false, false, false);
        assert!(claimed.is_ok(), "a retry after {} is admitted, not refused", status.as_str());
    }
}

/// Issue #673: a second live colony for one supply-chain target is refused at launch the way a
/// second colony on one issue is — `allow_duplicate` overrides it, and a finished holder blocks
/// nothing.
#[tokio::test]
async fn a_second_colony_for_one_supply_chain_target_is_refused_until_allow_duplicate() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    let mut holder = colony("acme", SessionStatus::Running);
    holder.id = "holder".into();
    holder.repo = "acme/app".into();
    holder.supply_chain = Some(crate::supersede::SupplyChainTarget::new("lodash", "ghsa-1"));
    app.sessions.write().await.push(holder);
    let request = |allow_duplicate: bool| {
        Json(NewSession {
            supply_chain: Some(crate::supersede::SupplyChainTarget::new("Lodash", "GHSA-1")),
            allow_duplicate,
            ..stack_request("acme/app", None, false).0
        })
    };

    let err = create(State(app.clone()), None, request(false)).await.unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT, "a live holder refuses the launch");
    let message = err.1.to_string();
    assert!(message.contains("holder") && message.contains("lodash / ghsa-1"), "{message}");
    assert!(message.contains("allow_duplicate"), "{message}");
    assert_eq!(app.sessions.read().await.len(), 1, "the refusal created nothing");

    // A target with a side missing is refused before anything is checked against it.
    let err = create(
        State(app.clone()),
        None,
        Json(NewSession {
            supply_chain: Some(crate::supersede::SupplyChainTarget::new("lodash", "  ")),
            ..request(false).0
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.0,
        StatusCode::BAD_REQUEST,
        "a target names both a package and an advisory"
    );
    assert_eq!(app.sessions.read().await.len(), 1);

    let created = create(State(app.clone()), None, request(true))
        .await
        .unwrap_or_else(|e| panic!("allow_duplicate starts a second colony anyway: {:#}", e.1));
    assert_eq!(
        created
            .supply_chain
            .as_ref()
            .map(|t| (t.package.as_str(), t.advisory.as_str())),
        Some(("lodash", "ghsa-1")),
        "the target is normalized onto the record"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn allow_duplicate_bypasses_the_hold_that_blocks_a_second_claim() {
    // The holder here is still live, unlike the finished colonies above: without
    // `allow_duplicate` the claim is refused with the holder handed back; passing
    // `allow_duplicate: true` for the same issue is admitted anyway.
    let mut sessions = vec![on_issue("first", 7, SessionStatus::Running)];

    let mut blocked = colony("acme", SessionStatus::Starting);
    blocked.id = "blocked".into();
    blocked.issue = Some(7);
    let refused = try_claim_session(&mut sessions, true, blocked, "acme/repo", Some(7), false, false, false);
    assert!(
        matches!(&refused, Err(held) if held.id == "first"),
        "a live holder refuses a second claim without allow_duplicate"
    );
    assert_eq!(sessions.len(), 1, "the refused claim inserted nothing");

    let mut second = colony("acme", SessionStatus::Starting);
    second.id = "second".into();
    second.issue = Some(7);
    let admitted = try_claim_session(&mut sessions, true, second, "acme/repo", Some(7), true, false, false);
    assert!(
        admitted.is_ok(),
        "allow_duplicate lets a second colony start on an issue another still holds"
    );
    assert_eq!(sessions.len(), 2, "the admitted duplicate is inserted alongside the holder");
}

/// Issue #673, the authoritative in-lock form: the supply-chain hold is re-checked beside the
/// issue hold, refuses a second colony for one target, and admits with `allow_duplicate`.
#[test]
fn the_in_lock_claim_refuses_a_second_colony_for_one_target_and_admits_with_allow_duplicate() {
    let mut holder = colony("acme", SessionStatus::Running);
    holder.id = "holder".into();
    holder.repo = "acme/repo".into();
    holder.supply_chain = Some(crate::supersede::SupplyChainTarget::new("lodash", "ghsa-1"));
    let mut sessions = vec![holder];
    let mut second = colony("acme", SessionStatus::Starting);
    second.id = "second".into();
    second.repo = "acme/repo".into();
    second.supply_chain = Some(crate::supersede::SupplyChainTarget::new("lodash", "ghsa-1"));
    assert!(
        crate::supersede::supply_chain_held_by(&sessions, "acme/repo", second.supply_chain.as_ref().unwrap()).is_some(),
        "the same hold the pre-check read holds under the lock"
    );
    assert!(
        matches!(
            try_claim_session(&mut sessions, true, second.clone(), "acme/repo", None, false, false, false),
            Err(held) if held.id == "holder"
        ),
        "a launch that did not ask to duplicate is refused with the holder"
    );
    assert_eq!(sessions.len(), 1, "the refused claim inserted nothing");
    let (admitted, _, _) = try_claim_session(&mut sessions, true, second, "acme/repo", None, true, false, false)
        .expect("allow_duplicate is admitted");
    assert_eq!(admitted.status, SessionStatus::Starting);
    assert_eq!(sessions.len(), 2, "the admitted duplicate sits beside the holder");
}

/// A `claim_wait` waiter for issue 7, queued behind `holder`, created `ago_secs` ago so the
/// oldest-first tiebreak is deterministic.
fn waiter_on_issue(id: &str, holder: &str, ago_secs: i64) -> Session {
    let mut s = colony("acme", SessionStatus::Queued);
    s.id = id.into();
    s.issue = Some(7);
    s.claim_wait = true;
    s.queued_behind = Some(holder.into());
    s.created_at = Utc::now() - chrono::Duration::seconds(ago_secs);
    s
}

#[test]
fn a_launch_asked_to_queue_waits_behind_the_holder_instead_of_being_refused() {
    // Issue #321: the polite third option — the default refuses, `allow_duplicate` duplicates,
    // `queue_behind_holder` admits the launch as a waiter behind the holder, even with a slot
    // free: its turn comes when the queue gets to it, not before.
    let mut sessions = vec![on_issue("holder", 7, SessionStatus::Running)];
    let mut polite = colony("acme", SessionStatus::Starting);
    polite.id = "polite".into();
    polite.issue = Some(7);
    let (admitted, queued, _) = try_claim_session(&mut sessions, true, polite, "acme/repo", Some(7), false, true, false)
        .expect("a waiter is admitted, not refused");
    assert!(
        queued && admitted.status == SessionStatus::Queued,
        "queued even with a free slot"
    );
    assert!(admitted.claim_wait, "the colony is a waiter for its issue");
    assert_eq!(admitted.queued_behind.as_deref(), Some("holder"), "queued behind the holder");
    assert_eq!(sessions.len(), 2, "the waiter is inserted");
    // And the holder still holds the issue: the waiter claims nothing while it waits.
    assert_eq!(
        issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
        Some("holder".to_string()),
        "the waiter does not take the hold over by waiting"
    );
}

#[test]
fn the_default_still_refuses_and_allow_duplicate_still_wins_over_queueing() {
    // The two existing launches are unchanged, and `allow_duplicate` takes precedence when a
    // request sets both: it starts now, it does not wait its turn.
    let mut sessions = vec![on_issue("holder", 7, SessionStatus::Running)];
    let mut plain = colony("acme", SessionStatus::Starting);
    plain.id = "plain".into();
    plain.issue = Some(7);
    assert!(
        matches!(
            try_claim_session(&mut sessions, true, plain, "acme/repo", Some(7), false, false, false),
            Err(held) if held.id == "holder"
        ),
        "a launch that did not ask to queue is refused as ever"
    );
    let mut duplicate = colony("acme", SessionStatus::Starting);
    duplicate.id = "duplicate".into();
    duplicate.issue = Some(7);
    let (admitted, queued, _) = try_claim_session(&mut sessions, true, duplicate, "acme/repo", Some(7), true, true, false)
        .expect("allow_duplicate bypasses the hold");
    assert!(!queued && admitted.status == SessionStatus::Starting, "starts, not waits");
    assert!(!admitted.claim_wait, "a duplicate is no waiter");
}

#[test]
fn with_only_waiters_left_the_oldest_one_holds_the_issue() {
    // The holder is gone; queue order decides. A fresh launch is refused naming the oldest
    // waiter — starting ahead of it would jump the queue, and waving the newcomer through
    // would duplicate the first waiter's work the moment its turn came.
    let sessions = vec![
        waiter_on_issue("first", "holder", 100),
        waiter_on_issue("second", "holder", 50),
    ];
    let held = issue_held_by(&sessions, "acme/repo", 7).expect("a waiter holds the issue once the holder is gone");
    assert_eq!(held.id, "first", "the oldest waiter holds it");
    let message = duplicate_message(&held, 7);
    assert!(
        message.contains("colony first is already on #7") && message.contains("allow_duplicate"),
        "{message}"
    );
    // A polite launch queues behind that same waiter, and the atomic claim refuses the
    // default one for it.
    let mut sessions = sessions;
    let mut fresh = colony("acme", SessionStatus::Starting);
    fresh.id = "fresh".into();
    fresh.issue = Some(7);
    assert!(
        matches!(
            try_claim_session(&mut sessions, true, fresh.clone(), "acme/repo", Some(7), false, false, false),
            Err(held) if held.id == "first"
        ),
        "a fresh default launch is refused naming the oldest waiter"
    );
    let (admitted, _, _) =
        try_claim_session(&mut sessions, true, fresh, "acme/repo", Some(7), false, true, false).expect("a polite launch waits");
    assert_eq!(admitted.queued_behind.as_deref(), Some("first"), "behind the oldest waiter");
}

#[test]
fn a_real_holder_outranks_the_waiters_however_young_it_is() {
    // The first non-waiter holder wins whatever the creation order: waiters only take over
    // once there is no holder left at all.
    let mut holder = on_issue("holder", 7, SessionStatus::Running);
    holder.created_at = Utc::now();
    let sessions = vec![waiter_on_issue("old-waiter", "gone", 200), holder];
    assert_eq!(
        issue_held_by(&sessions, "acme/repo", 7).map(|s| s.id),
        Some("holder".to_string()),
        "the holder keeps the issue; the waiter keeps waiting"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[allow(clippy::result_large_err)]
async fn two_simultaneous_claims_on_one_issue_let_exactly_one_through() {
    // The TOCTOU window this guards: two launches both passing the read-locked pre-check before
    // either inserts. Both collide here inside the write lock instead, through the same
    // `try_claim_session` the handler admits with — one is admitted, the other gets its holder.
    let sessions = std::sync::Arc::new(RwLock::new(Vec::new()));
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for i in 0..2 {
        let (sessions, barrier) = (sessions.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            let mut fresh = colony("acme", SessionStatus::Starting);
            fresh.id = format!("racer-{i}");
            fresh.issue = Some(7);
            with_slot(&sessions, "acme", "acme/repo", 8, None, 8, |guard, room| {
                try_claim_session(guard, room, fresh, "acme/repo", Some(7), false, false, false)
            })
            .await
        }));
    }
    let mut admitted = 0;
    let mut refused = 0;
    for task in tasks {
        match task.await.expect("claim task joined") {
            Ok(_) => admitted += 1,
            Err(_) => refused += 1,
        }
    }
    assert_eq!(admitted, 1, "exactly one racer is admitted");
    assert_eq!(refused, 1, "the other gets the holder back for its 409");
    let done = sessions.read().await;
    assert_eq!(done.len(), 1, "the loser inserted nothing");
    let held = issue_held_by(&done, "acme/repo", 7).expect("the winner holds #7");
    assert!(
        held.id == "racer-0" || held.id == "racer-1",
        "the holder is the admitted racer, not a stranger: {}",
        held.id
    );
    // And the loser's 409 reads the way the handler's does.
    let message = duplicate_message(&held, 7);
    assert!(
        message.contains(&format!("colony {} is already on #7", held.id)) && message.contains("allow_duplicate"),
        "{message}"
    );
}

/// A throwaway App whose config switches the `acme` workspace off, the way an old install's
/// `orgs.json` plus one settings save leaves it.
async fn app_with_org_switched_off(id: &str, status: SessionStatus) -> (Shared, PathBuf) {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = test_app(&root);
    std::fs::create_dir_all(app.cfg.config_dir.clone()).unwrap();
    std::fs::write(app.cfg.config_dir.join("orgs.json"), r#"{"acme": {"enabled": false}}"#).unwrap();
    let mut s = colony("acme", status);
    s.id = id.to_string();
    s.git_admin_dir = Some("git".into());
    app.sessions.write().await.push(s);
    tokio::fs::create_dir_all(app.session_dir(id)).await.unwrap();
    (app, root)
}

#[tokio::test]
async fn a_switched_off_org_refuses_new_colonies_and_names_the_way_back_on() {
    let (app, root) = app_with_org_switched_off("kept", SessionStatus::Stopped).await;
    let err = create(
        State(app.clone()),
        None,
        Json(NewSession {
            repo: "acme/app".into(),
            issue: None,
            title: String::new(),
            instructions: String::new(),
            autopilot: None,
            verify: None,
            autofix: None,
            automerge: None,
            allow_duplicate: false,
            allow_epic: false,
            queue_behind_holder: false,
            supply_chain: None,
            model_tier: None,
            model_override: None,
            subagent_model_override: None,
            claude_account: None,
            after: None,
            stack: false,
            origin: None,
            host: None,
            serialize: None,
            handoff: None,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    let message = err.1.to_string();
    assert!(message.contains("acme"), "{message}");
    assert!(message.contains("switched off"), "{message}");
    assert!(
        message.contains("org settings"),
        "the message says what to do about it: {message}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn colonies_of_a_switched_off_org_stay_listed_and_resume() {
    let (app, root) = app_with_org_switched_off("kept", SessionStatus::Stopped).await;
    let listed = list_bare(State(app.clone()), None).await.0;
    let kept = listed.iter().find(|s| s.id == "kept").unwrap();
    assert_eq!(kept.org, "acme", "the colony is still in the list");
    // The real resume path, not just its gate: the org's switch does not make `resume` refuse
    // the colony — it is claimed and handed to a fresh boot like any other. The boot itself
    // never runs here: the spawned task is dropped with the one-thread test runtime before it
    // is polled, so nothing reaches for GitHub or a microVM.
    let resumed = resume(State(app.clone()), Path("kept".into()), None)
        .await
        .unwrap_or_else(|e| panic!("resume refused a colony of a switched-off org: {:#}", e.1))
        .0;
    assert_eq!(resumed.id, "kept");
    assert_eq!(
        resumed.status,
        SessionStatus::Starting,
        "the resume claimed the colony and started a boot"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn starting_a_colony_marks_its_org_known_so_the_operator_is_never_asked_about_it() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    // The smallest install `create` insists on: an agent module matching the configured provider
    // and a guest binary that claims to be an ELF.
    let assets = root.join("assets");
    let dir = assets.join("modules/agents/claude-code");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("module.json"), r#"{"id":"claude-code","entry":["run"]}"#).unwrap();
    std::fs::create_dir_all(assets.join("bin")).unwrap();
    std::fs::write(assets.join("bin/colonizer-agentd"), b"\x7fELF padding").unwrap();
    let agent = AgentModule::test("claude-code")
        .name("Claude Code")
        .dir(dir)
        .entry(vec!["run".into()]);
    let app = crate::tests::test_app_with_agents(&root, vec![agent], |cfg| cfg.assets = Some(assets));
    // The org is still awaiting an answer when the colony starts, sighting and avatar both.
    *app.new_orgs.write().await =
        std::collections::BTreeMap::from([("acme".to_string(), Some("https://a/acme.png".to_string()))]);

    let created = create(
        State(app.clone()),
        None,
        Json(NewSession {
            repo: "acme/app".into(),
            issue: None,
            title: String::new(),
            instructions: String::new(),
            autopilot: None,
            verify: None,
            autofix: None,
            automerge: None,
            allow_duplicate: false,
            allow_epic: false,
            queue_behind_holder: false,
            supply_chain: None,
            model_tier: None,
            model_override: None,
            subagent_model_override: None,
            claude_account: None,
            after: None,
            stack: false,
            origin: None,
            host: None,
            serialize: None,
            handoff: None,
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("create refused: {:#}", e.1));
    assert_eq!(created.org, "acme");
    assert_eq!(
        app.known_orgs().unwrap().get("acme").cloned(),
        Some(crate::orgs::KnownOrg {
            avatar_url: Some("https://a/acme.png".into()),
        }),
        "working in an org is an answer, and the sighting's avatar is recorded with it; the prompt \
             must never ask about it later"
    );
    let _ = std::fs::remove_dir_all(root);
}

// -- stacking (create with `after`) ----------------------------------------------------------

/// A `create` request with nothing but the repo and, where the test names one, the parent.
/// Unstacked unless the test says otherwise: the default queues behind the parent's merge.
fn stack_request(repo: &str, after: Option<String>, stack: bool) -> Json<NewSession> {
    Json(NewSession {
        repo: repo.into(),
        issue: None,
        title: String::new(),
        instructions: String::new(),
        autopilot: None,
        verify: None,
        autofix: None,
        automerge: None,
        allow_duplicate: false,
        allow_epic: false,
        queue_behind_holder: false,
        supply_chain: None,
        model_tier: None,
        model_override: None,
        subagent_model_override: None,
        claude_account: None,
        after,
        stack,
        origin: None,
        host: None,
        serialize: None,
        handoff: None,
    })
}

#[tokio::test]
async fn a_colony_asked_to_stack_on_another_queues_until_that_one_pushes() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    let mut parent = colony("acme", SessionStatus::Running);
    parent.id = "parent".into();
    parent.branch = "colonizer/issue-1-parent".into();
    parent.repo = "acme/app".into();
    app.sessions.write().await.push(parent);

    let created = create(
        State(app.clone()),
        None,
        stack_request("acme/app", Some("parent".into()), true),
    )
    .await
    .unwrap_or_else(|e| panic!("create refused a stacked colony: {:#}", e.1));
    assert_eq!(
        created.parent.as_deref(),
        Some("parent"),
        "the colony records what it is stacked on"
    );
    assert_eq!(
        created.status,
        SessionStatus::Queued,
        "the parent has not pushed a branch, so the colony queues even though a slot is free"
    );
    assert_eq!(
        created.base, None,
        "the base is the boot's business, resolved fresh when the wait ends"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A parent colony for the queue-by-default tests below: open issue, same repository.
async fn parent_on(app: &Shared, status: SessionStatus) {
    let mut parent = colony("acme", status);
    parent.id = "parent".into();
    parent.branch = "colonizer/issue-1-parent".into();
    parent.repo = "acme/app".into();
    app.sessions.write().await.push(parent);
}

#[tokio::test]
async fn by_default_a_colony_behind_an_open_pull_request_queues_for_the_merge() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    parent_on(&app, SessionStatus::PrOpened).await;

    let created = create(
        State(app.clone()),
        None,
        stack_request("acme/app", Some("parent".into()), false),
    )
    .await
    .unwrap_or_else(|e| panic!("create refused a queued colony: {:#}", e.1));
    assert_eq!(created.parent.as_deref(), Some("parent"));
    assert!(!created.stack, "queueing, not stacking, is the default");
    assert_eq!(
        created.status,
        SessionStatus::Queued,
        "the parent's pull request is still open, so the colony queues even though a slot is free"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_explicit_stack_starts_from_the_open_pull_request() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    parent_on(&app, SessionStatus::PrOpened).await;

    let created = create(
        State(app.clone()),
        None,
        stack_request("acme/app", Some("parent".into()), true),
    )
    .await
    .unwrap_or_else(|e| panic!("create refused a stacked colony: {:#}", e.1));
    assert_eq!(created.parent.as_deref(), Some("parent"));
    assert!(created.stack);
    assert_eq!(
        created.status,
        SessionStatus::Starting,
        "the parent's branch is pushed, so an explicit stack starts at once"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn by_default_a_colony_behind_a_merged_parent_starts_at_once() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    parent_on(&app, SessionStatus::Merged).await;

    let created = create(
        State(app.clone()),
        None,
        stack_request("acme/app", Some("parent".into()), false),
    )
    .await
    .unwrap_or_else(|e| panic!("create refused a queued colony: {:#}", e.1));
    assert_eq!(
        created.status,
        SessionStatus::Starting,
        "the parent's work is already merged, so there is nothing to wait for"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn by_default_a_colony_behind_a_closed_parent_is_refused_naming_the_stack_flag() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    parent_on(&app, SessionStatus::Closed).await;

    let err = create(
        State(app.clone()),
        None,
        stack_request("acme/app", Some("parent".into()), false),
    )
    .await
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);
    let message = err.1.to_string();
    assert!(message.contains("parent"), "{message}");
    assert!(
        message.contains("stack: true"),
        "the refusal says how to stack anyway: {message}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn create_refuses_to_stack_on_a_colony_that_can_never_lend_a_branch() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    let mut dead = colony("acme", SessionStatus::Failed);
    dead.id = "dead".into();
    dead.branch = "colonizer/issue-1-dead".into();
    dead.repo = "acme/app".into();
    app.sessions.write().await.push(dead);

    let err = create(State(app.clone()), None, stack_request("acme/app", Some("dead".into()), true))
        .await
        .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT, "a refusal, like the duplicate-issue one");
    let message = err.1.to_string();
    assert!(message.contains("dead"), "{message}");
    assert!(message.contains("failed"), "it says which reason applies: {message}");
    let sessions = app.sessions.read().await;
    assert_eq!(sessions.len(), 1, "nothing was created");
    drop(sessions);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn stacking_on_a_colony_that_does_not_exist_is_refused_as_a_404_naming_it() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    let err = create(
        State(app.clone()),
        None,
        stack_request("acme/app", Some("ghost".into()), true),
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.0,
        StatusCode::NOT_FOUND,
        "the same answer asking for an unknown colony gets"
    );
    let message = err.1.to_string();
    assert!(message.contains("ghost") && message.contains("no colony"), "{message}");
    assert!(app.sessions.read().await.is_empty(), "nothing was created");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_after_of_nothing_but_whitespace_is_refused_not_read_as_absent() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    let err = create(State(app.clone()), None, stack_request("acme/app", Some("   ".into()), true))
        .await
        .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST, "the request names nothing stackable");
    assert!(err.1.to_string().contains("`after`"), "{}", err.1);
    assert!(
        app.sessions.read().await.is_empty(),
        "silently starting unstacked would branch from the wrong place without a word"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn stacking_on_a_colony_of_another_repository_is_refused_naming_both() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    let mut parent = colony("acme", SessionStatus::PrOpened);
    parent.id = "parent".into();
    parent.branch = "colonizer/issue-1-parent".into();
    parent.repo = "acme/app".into();
    app.sessions.write().await.push(parent);

    let err = create(
        State(app.clone()),
        None,
        stack_request("acme/other", Some("parent".into()), true),
    )
    .await
    .unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT, "a refusal at create, like the other stack ones");
    let message = err.1.to_string();
    assert!(message.contains("acme/app"), "the parent's repository is named: {message}");
    assert!(message.contains("acme/other"), "and so is this one's: {message}");
    let sessions = app.sessions.read().await;
    assert_eq!(sessions.len(), 1, "nothing was created to fail a boot later");
    drop(sessions);
    let _ = std::fs::remove_dir_all(root);
}

// -- launch refusals (create's bad-request gates) ---------------------------------------------

/// An unknown tier would silently fall back to the routing rule's own choice, which is not what
/// an operator naming one asked for, so `create` refuses naming the tier and the ones there are.
#[tokio::test]
async fn an_unknown_model_tier_is_refused_naming_the_tier_and_the_known_ones() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);

    let err = create(
        State(app.clone()),
        None,
        Json(NewSession {
            model_tier: Some("gigantic".into()),
            ..stack_request("acme/app", None, false).0
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST, "the request names a tier that does not exist");
    let message = err.1.to_string();
    assert!(message.contains("gigantic"), "{message}");
    assert!(
        message.contains("low, medium or high"),
        "it names the tiers there are: {message}"
    );
    assert!(app.sessions.read().await.is_empty(), "nothing was created");
    let _ = std::fs::remove_dir_all(root);
}

/// A `<provider>/<model>` override no configured provider owns would only fail inside the
/// colony, so `create` refuses at launch naming the override and the provider it names — for
/// the orchestrator's model and the subagents' alike.
#[tokio::test]
async fn a_model_override_naming_no_configured_provider_is_refused_naming_both() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);

    let err = create(
        State(app.clone()),
        None,
        Json(NewSession {
            model_override: Some("unconfigured/claude-opus-4".into()),
            ..stack_request("acme/app", None, false).0
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    let message = err.1.to_string();
    assert!(message.contains("unconfigured/claude-opus-4"), "{message}");
    assert!(
        message.contains("no configured provider \"unconfigured\""),
        "it names the provider nothing configures: {message}"
    );

    // The subagent override rides the same check, under its own name.
    let err = create(
        State(app.clone()),
        None,
        Json(NewSession {
            subagent_model_override: Some("unconfigured/claude-opus-4".into()),
            ..stack_request("acme/app", None, false).0
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    assert!(err.1.to_string().contains("subagent model"), "{}", err.1);
    assert!(app.sessions.read().await.is_empty(), "nothing was created");
    let _ = std::fs::remove_dir_all(root);
}

/// The agent module a launch would run on must be installed: an org pointed at one no manifest
/// answers for is refused at launch, not discovered from a failed boot.
#[tokio::test]
async fn a_launch_on_an_agent_module_that_is_not_installed_is_refused() {
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create(&root);
    app.modules.write().await.agent.provider = "ghost-module".into();

    let err = create(State(app.clone()), None, stack_request("acme/app", None, false))
        .await
        .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    let message = err.1.to_string();
    assert!(message.contains("not installed"), "{message}");
    assert!(app.sessions.read().await.is_empty(), "nothing was created");
    let _ = std::fs::remove_dir_all(root);
}

/// An agent module that runs on Claude refuses to launch without a credential to run on: the
/// refusal names the account the operator would log in with.
#[tokio::test]
async fn a_launch_without_claude_credentials_is_refused_naming_the_account() {
    // `claude_cred_for` falls back to the environment, so where a token is exported the launch
    // would rightly start and the refusal cannot be arranged: skip rather than misreport.
    if crate::util::env_nonempty("CLAUDE_CODE_OAUTH_TOKEN").is_some() || crate::util::env_nonempty("ANTHROPIC_API_KEY").is_some()
    {
        return;
    }
    let root = std::env::temp_dir().join(format!("colonizer-sessions-{}", short_id()));
    let app = app_that_can_create_needing(&root, true);

    let err = create(State(app.clone()), None, stack_request("acme/app", None, false))
        .await
        .unwrap_err();
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    let message = err.1.to_string();
    assert!(message.contains("log in with Claude"), "{message}");
    assert!(message.contains("'default'"), "it names the account: {message}");
    assert!(app.sessions.read().await.is_empty(), "nothing was created");
    let _ = std::fs::remove_dir_all(root);
}
