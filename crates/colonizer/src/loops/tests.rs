use super::*;
use crate::retry::FailureClass;
use crate::sessions::tests::colony;
use chrono::TimeZone;

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
}

fn a_loop(cadence: Cadence) -> Loop {
    Loop {
        id: "loop_a".into(),
        name: "Triage".into(),
        org: "acme".into(),
        repo: "acme/web".into(),
        created_by_token: None,
        pending: Vec::new(),
        prompt: "Triage new issues".into(),
        cadence,
        kind: LoopKind::Colony,
        needs_github: false,
        tz_offset_minutes: 120,
        model: None,
        subagent_model: None,
        autopilot: true,
        max_runs: None,
        retry_failed_runs: None,
        end_at: None,
        enabled: true,
        next_run_at: Some(utc(2026, 9, 24, 9, 0)),
        runs: 0,
        last_run: None,
        last_note: None,
        ended_reason: None,
        created_at: utc(2026, 9, 1, 0, 0),
        disk_cleanup: None,
    }
}

#[test]
fn origin_tags_name_their_loop() {
    assert_eq!(loop_id_of("loop:loop_a"), Some("loop_a"));
    assert_eq!(loop_id_of("loop:"), None);
    assert_eq!(loop_id_of("burn_down"), None);
}

/// Issue #778: the gate is asked for a GitHub colony loop and nothing else, and a GitHub loop's
/// brief says where its inputs are (and that it has no `gh`). Pure — `loop_github` owns the note
/// the gate leaves, `github.mjs` the tool list.
#[test]
fn a_github_loop_is_gated_on_github_and_told_where_its_inputs_are() {
    let mut l = a_loop(Cadence::Interval { minutes: 60 });
    assert!(!needs_preflight(&l), "a loop that did not ask is not gated");
    l.needs_github = true;
    assert!(needs_preflight(&l));

    let brief = loop_instructions(&l, 1, false);
    assert!(brief.contains("/colonizer/github"), "{brief}");
    assert!(brief.contains("issues.json") && brief.contains("merged-prs.json"), "{brief}");
    assert!(brief.contains("you have no GitHub token"), "{brief}");

    // The need is a colony loop's only: a map loop carries it dropped, never gated.
    let mut map = l.clone();
    map.kind = LoopKind::Map;
    assert!(!needs_preflight(&map));
}

#[tokio::test]
async fn a_map_loop_may_cover_the_org_without_a_prompt_a_colony_loop_may_not() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-from-{}", short_id()));
    let app = crate::tests::test_app(&root);
    let now = utc(2026, 9, 24, 9, 0);
    let req = |repo: &str, kind: LoopKind, prompt: &str| NewLoop {
        name: "Keep the maps fresh".into(),
        repo: repo.into(),
        prompt: prompt.into(),
        cadence: Cadence::EveryDays {
            days: 14,
            hour: 3,
            minute: 0,
        },
        kind,
        needs_github: false,
        tz_offset_minutes: None,
        model: None,
        subagent_model: None,
        autopilot: None,
        max_runs: None,
        retry_failed_runs: None,
        end_at: None,
        enabled: None,
        disk_cleanup: None,
    };
    let map = loop_from(&app, req("acme/*", LoopKind::Map, ""), "loop_m".into(), now, now).unwrap();
    assert_eq!(
        (map.kind, map.repo.as_str(), map.org.as_str(), map.prompt.as_str()),
        (LoopKind::Map, "acme/*", "acme", ""),
        "the prompt is ignored and the pending list is server-owned"
    );
    assert!(map.pending.is_empty());
    // A map loop has no use for the GitHub need (issue #778): it is dropped, not carried.
    let mut wanted = req("acme/*", LoopKind::Map, "");
    wanted.needs_github = true;
    assert!(!loop_from(&app, wanted, "loop_g".into(), now, now).unwrap().needs_github);

    let err = loop_from(&app, req("acme/*", LoopKind::Colony, "Triage"), "loop_c".into(), now, now).unwrap_err();
    assert!(err.message().contains("owner/* is only for map loops"), "{}", err.message());

    let mut too_long = req("acme/web", LoopKind::Colony, "Triage");
    too_long.cadence = Cadence::EveryDays {
        days: 366,
        hour: 3,
        minute: 0,
    };
    let err = loop_from(&app, too_long, "loop_d".into(), now, now).unwrap_err();
    assert!(err.message().contains("1 to 365 days"), "{}", err.message());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_switched_off_org_skips_its_whole_cycle_with_one_note() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-off-{}", short_id()));
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/orgs.json"), json!({"acme": {"enabled": false}}).to_string()).unwrap();
    let app = crate::tests::test_app(&root);
    let mut l = a_loop(Cadence::EveryDays {
        days: 14,
        hour: 3,
        minute: 0,
    });
    l.kind = LoopKind::Map;
    l.repo = "acme/*".into();
    app.loops.loops.write().await.push(l);
    let now = utc(2026, 9, 24, 9, 0);
    let started = fire_map(&app, &app.loops.get("loop_a").await.unwrap(), now).await.unwrap();
    assert!(started.is_none(), "nothing was launched");
    let l = app.loops.get("loop_a").await.unwrap();
    assert!(l.last_note.unwrap().contains("switched off"), "the note says why");
    assert!(l.pending.is_empty(), "no cycle was started for the dead org");
    assert_eq!(l.runs, 0);
    assert_eq!(
        l.next_run_at,
        Some(utc(2026, 10, 8, 3, 0)),
        "the cycle waits for its next slot"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn map_loops_round_trip_and_loops_saved_before_kinds_read_as_colony_loops() {
    let mut map = a_loop(Cadence::EveryDays {
        days: 14,
        hour: 3,
        minute: 0,
    });
    map.kind = LoopKind::Map;
    map.repo = "acme/*".into();
    map.prompt = String::new();
    map.pending = vec!["acme/api".into()];
    map.created_by_token = Some("tok_x".into());
    let json = serde_json::to_value(&map).unwrap();
    assert_eq!(json["kind"], "map");
    assert_eq!(json["pending"], json!(["acme/api"]));
    assert_eq!(json["created_by_token"], "tok_x", "a token's loop keeps its token on disk");
    assert_eq!(serde_json::from_value::<Loop>(json).unwrap(), map);

    let mut old = serde_json::to_value(a_loop(Cadence::Interval { minutes: 60 })).unwrap();
    old.as_object_mut().unwrap().remove("kind");
    old.as_object_mut().unwrap().remove("created_by_token");
    let l: Loop = serde_json::from_value(old).unwrap();
    assert_eq!(l.kind, LoopKind::Colony, "no kind field means the default");
    assert_eq!(l.created_by_token, None, "a loop saved before tokens is the owner's");
    assert!(l.pending.is_empty());
}

#[test]
fn an_org_cycle_staggers_its_repositories_then_waits_for_the_next_slot() {
    let now = utc(2026, 9, 24, 9, 0);
    let cadence = Cadence::EveryDays {
        days: 14,
        hour: 3,
        minute: 0,
    };
    let stagger = now + ChronoDuration::minutes(MAP_STAGGER_MINUTES);
    let mut pending: Vec<String> = Vec::new();
    let (repo, next) = fan_out_step(
        &mut pending,
        || vec!["acme/web".into(), "acme/api".into(), "acme/cli".into()],
        &cadence,
        now,
    );
    assert_eq!(repo.as_deref(), Some("acme/web"), "the first step fills from the fresh list");
    assert_eq!(pending, ["acme/api", "acme/cli"]);
    assert_eq!(next, stagger);
    let (repo, next) = fan_out_step(&mut pending, Vec::new, &cadence, now);
    assert_eq!(repo.as_deref(), Some("acme/api"));
    assert_eq!(pending, ["acme/cli"]);
    assert_eq!(next, stagger, "middle steps stay a stagger apart");
    let (repo, next) = fan_out_step(&mut pending, Vec::new, &cadence, now);
    assert_eq!(repo.as_deref(), Some("acme/cli"));
    assert!(pending.is_empty());
    assert_eq!(next, utc(2026, 10, 8, 3, 0), "after the last one, the cadence's next slot");
}

#[test]
fn a_new_cycle_relists_the_org_so_later_repositories_join_in() {
    let now = utc(2026, 9, 24, 9, 0);
    let cadence = Cadence::EveryDays {
        days: 14,
        hour: 3,
        minute: 0,
    };
    let mut pending: Vec<String> = vec!["acme/api".into()];
    let (repo, next) = fan_out_step(&mut pending, Vec::new, &cadence, now);
    assert_eq!(repo.as_deref(), Some("acme/api"), "the old cycle finishes first");
    assert_eq!(next, utc(2026, 10, 8, 3, 0));
    let (repo, next) = fan_out_step(&mut pending, || vec!["acme/web".into(), "acme/new".into()], &cadence, now);
    assert_eq!(repo.as_deref(), Some("acme/web"));
    assert_eq!(pending, ["acme/new"], "the repository added since is in this cycle");
    assert_eq!(next, now + ChronoDuration::minutes(MAP_STAGGER_MINUTES));
}

#[test]
fn a_map_loop_is_held_by_its_mapping_colony_but_an_org_wide_one_is_not() {
    let mut sessions = Vec::new();
    let mut drawing = colony("acme", SessionStatus::Running);
    drawing.id = "m1".into();
    drawing.origin = Some(crate::maps::loop_origin("loop_a"));
    sessions.push(drawing);
    let mut repo_loop = a_loop(Cadence::EveryDays {
        days: 14,
        hour: 3,
        minute: 0,
    });
    repo_loop.kind = LoopKind::Map;
    repo_loop.repo = "acme/web".into();
    assert_eq!(live_run_for(&sessions, &repo_loop).map(|s| s.id.as_str()), Some("m1"));
    let mut org_loop = repo_loop.clone();
    org_loop.repo = "acme/*".into();
    assert!(
        live_run_for(&sessions, &org_loop).is_none(),
        "an org-wide cycle never holds itself"
    );
    assert!(live_run_for(&sessions, &a_loop(Cadence::Daily { hour: 9, minute: 0 })).is_none());
}

#[test]
fn due_loops_are_enabled_and_not_in_the_future() {
    let now = utc(2026, 9, 24, 9, 0);
    let mut later = a_loop(Cadence::Daily { hour: 9, minute: 0 });
    later.id = "later".into();
    later.next_run_at = Some(now + ChronoDuration::minutes(1));
    let mut off = a_loop(Cadence::Daily { hour: 9, minute: 0 });
    off.id = "off".into();
    off.enabled = false;
    let mut ended = a_loop(Cadence::Daily { hour: 9, minute: 0 });
    ended.id = "ended".into();
    ended.next_run_at = None;
    let now_due = a_loop(Cadence::Daily { hour: 9, minute: 0 });
    assert_eq!(due(&[later, off, ended, now_due], now), vec!["loop_a".to_string()]);
}

#[test]
fn a_live_previous_run_skips_the_tick_and_says_so() {
    let now = utc(2026, 9, 24, 9, 0);
    let daily = a_loop(Cadence::Daily { hour: 9, minute: 0 });
    assert_eq!(plan_tick(&daily, None, now), Tick::Launch);
    match plan_tick(&daily, Some("abc123"), now) {
        Tick::Skip { until, note } => {
            assert_eq!(until, utc(2026, 9, 25, 9, 0), "a fixed loop waits for its next slot");
            assert!(note.contains("abc123") && note.contains("still live"), "{note}");
        }
        other => panic!("expected a skip, got {other:?}"),
    }
    let paced = a_loop(Cadence::SelfPaced {});
    match plan_tick(&paced, Some("abc123"), now) {
        Tick::Skip { until, .. } => assert_eq!(until, now + ChronoDuration::minutes(15), "self-paced retries soon"),
        other => panic!("expected a skip, got {other:?}"),
    }
}

#[test]
fn only_in_flight_colonies_count_as_the_live_run() {
    let mut sessions = Vec::new();
    for (id, status) in [
        ("done", SessionStatus::Merged),
        ("opened", SessionStatus::PrOpened),
        ("stopped", SessionStatus::Stopped),
    ] {
        let mut s = colony("acme", status);
        s.id = id.into();
        s.origin = Some("loop:loop_a".into());
        sessions.push(s);
    }
    assert!(live_run(&sessions, "loop_a").is_none(), "finished runs do not hold the loop");
    let mut other = colony("acme", SessionStatus::Running);
    other.id = "other-loop".into();
    other.origin = Some("loop:loop_b".into());
    sessions.push(other);
    assert!(
        live_run(&sessions, "loop_a").is_none(),
        "another loop's run is not this one's"
    );
    let mut queued = colony("acme", SessionStatus::Queued);
    queued.id = "queued".into();
    queued.origin = Some("loop:loop_a".into());
    sessions.push(queued);
    assert_eq!(live_run(&sessions, "loop_a").map(|s| s.id.as_str()), Some("queued"));
}

#[test]
fn a_run_books_the_next_and_limits_end_the_loop() {
    let now = utc(2026, 9, 24, 9, 0);
    let mut l = a_loop(Cadence::Interval { minutes: 60 });
    l.max_runs = Some(2);
    l.record_run("s1", now);
    assert_eq!((l.runs, l.enabled), (1, true));
    assert_eq!(l.next_run_at, Some(utc(2026, 9, 24, 10, 0)));
    l.record_run("s2", utc(2026, 9, 24, 10, 0));
    assert!(!l.enabled && l.next_run_at.is_none());
    assert_eq!(l.ended_reason.as_deref(), Some("finished: ran 2 times"));

    let mut dated = a_loop(Cadence::Daily { hour: 9, minute: 0 });
    dated.end_at = Some(utc(2026, 9, 25, 8, 0));
    dated.record_run("s1", now);
    assert!(!dated.enabled, "the next run (tomorrow 09:00) is past the end date");
    assert!(dated.ended_reason.unwrap().starts_with("finished: its end date"));
}

#[test]
fn next_delays_are_clamped_and_described() {
    assert_eq!(clamp_next(1), 15);
    assert_eq!(clamp_next(90), 90);
    assert_eq!(clamp_next(10_000), 1440);
    assert_eq!(human_minutes(60), "1 hour");
    assert_eq!(human_minutes(90), "1h 30m");
    assert_eq!(human_minutes(30), "30 minutes");
}

#[test]
fn colonies_are_told_their_run_and_how_to_pace_or_stop() {
    let paced = a_loop(Cadence::SelfPaced {});
    let text = loop_instructions(&paced, 3, true);
    assert!(text.starts_with("Triage new issues"));
    assert!(text.contains("run 3 of the loop \"Triage\""));
    assert!(text.contains("loop_next") && text.contains("loop_stop"));
    let fixed = loop_instructions(&a_loop(Cadence::Daily { hour: 9, minute: 0 }), 1, true);
    assert!(!fixed.contains("call loop_next") && fixed.contains("loop_stop"));

    // A module without the loop tools (issue #643) is never told to call them, and a
    // self-paced loop just says when it comes round again.
    let plain_paced = loop_instructions(&paced, 3, false);
    assert!(!plain_paced.contains("loop_next") && !plain_paced.contains("loop_stop"));
    assert!(plain_paced.contains("This loop is self-paced: it runs again in 24 hours."));
    let plain_fixed = loop_instructions(&a_loop(Cadence::Daily { hour: 9, minute: 0 }), 1, false);
    assert!(!plain_fixed.contains("loop_next") && !plain_fixed.contains("loop_stop"));
    assert!(plain_fixed.contains("fixed schedule"));
}

#[tokio::test]
async fn a_launch_briefs_its_colony_for_its_module_s_loop_tools() {
    // The launch resolves the org's effective agent module — the one `sessions::create` is
    // about to launch on — and names the loop tools only when that module serves them.
    for tools in [true, false] {
        let root = std::env::temp_dir().join(format!("colonizer-loops-brief-{tools}-{}", short_id()));
        let mut app = crate::sessions::tests::app_that_can_create(&root);
        std::sync::Arc::get_mut(&mut app).unwrap().agents[0].loop_tools = tools;
        let mut l = a_loop(Cadence::SelfPaced {});
        l.repo = "acme/app".into();
        app.loops.loops.write().await.push(l);
        let session = launch(&app, &app.loops.get("loop_a").await.unwrap(), utc(2026, 9, 24, 9, 0))
            .await
            .unwrap();
        let brief = session.instructions.as_str();
        assert_eq!(brief.contains("loop_next"), tools, "{tools}: {brief}");
        assert_eq!(brief.contains("loop_stop"), tools, "{tools}: {brief}");
        if !tools {
            assert!(brief.contains("it runs again in 24 hours"), "{tools}: {brief}");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[tokio::test]
async fn loops_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("colonizer-loops-{}", short_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = LoopStore::new(&dir);
    store.loops.write().await.push(a_loop(Cadence::Weekly {
        weekday: 3,
        hour: 9,
        minute: 30,
    }));
    store.save().await.unwrap();
    let again = LoopStore::new(&dir);
    let loaded = again.get("loop_a").await.unwrap();
    assert_eq!(
        loaded,
        a_loop(Cadence::Weekly {
            weekday: 3,
            hour: 9,
            minute: 30
        })
    );
    let raw: Value = serde_json::from_slice(&std::fs::read(dir.join("loops.json")).unwrap()).unwrap();
    let saved = raw.as_array().unwrap().iter().find(|l| l["id"] == "loop_a").unwrap();
    assert_eq!(
        raw.as_array().unwrap().len(),
        1,
        "loops.json holds only the operator's loops, so an older build still reads it: {raw}"
    );
    assert_eq!(
        again.get(crate::disk_cleanup::LOOP_ID).await.map(|l| l.kind),
        Some(LoopKind::DiskCleanup),
        "the built-in loop comes back too"
    );
    assert_eq!(
        saved["cadence"],
        json!({"every": "weekly", "weekday": 3, "hour": 9, "minute": 30})
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_builtin_disk_cleanup_keeps_its_switch_in_its_own_file() {
    let dir = std::env::temp_dir().join(format!("colonizer-loops-builtin-{}", short_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = LoopStore::new(&dir);
    assert!(!dir.join("loops.json").exists(), "a fresh install writes nothing");
    store
        .update(crate::disk_cleanup::LOOP_ID, |l| l.enabled = true)
        .await
        .unwrap();
    assert!(dir.join(crate::disk_cleanup::FILE).exists());
    let raw: Value = serde_json::from_slice(&std::fs::read(dir.join("loops.json")).unwrap()).unwrap();
    assert_eq!(raw, json!([]), "the built-in never lands in loops.json");
    let again = LoopStore::new(&dir);
    let builtin = again.get(crate::disk_cleanup::LOOP_ID).await.unwrap();
    assert!(builtin.enabled, "its switch survives a restart");
    assert_eq!(again.loops.read().await.len(), 1, "and there is only one");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_colony_paces_and_stops_its_own_loop() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-app-{}", short_id()));
    let app = crate::tests::test_app(&root);
    let mut paced = a_loop(Cadence::SelfPaced {});
    paced.next_run_at = None;
    app.loops.loops.write().await.push(paced);
    let mut s = colony("acme", SessionStatus::Running);
    s.id = "run1".into();
    s.origin = Some("loop:loop_a".into());
    app.sessions.write().await.push(s);

    let before = Utc::now();
    on_next(&app, "run1", 120, "CI reruns at 11").await;
    let l = app.loops.get("loop_a").await.unwrap();
    let next = l.next_run_at.expect("booked");
    assert!(next >= before + ChronoDuration::minutes(119) && next <= Utc::now() + ChronoDuration::minutes(121));
    assert!(l.last_note.unwrap().contains("2 hours: CI reruns at 11"));

    on_stop(&app, "run1", "all flakes fixed").await;
    let l = app.loops.get("loop_a").await.unwrap();
    assert!(!l.enabled && l.next_run_at.is_none());
    assert_eq!(l.ended_reason.as_deref(), Some("stopped by the colony: all flakes fixed"));

    // A colony no loop launched cannot touch loops.
    let mut stray = colony("acme", SessionStatus::Running);
    stray.id = "stray".into();
    app.sessions.write().await.push(stray);
    on_stop(&app, "stray", "nope").await;
    let _ = std::fs::remove_dir_all(root);
}

// -- Token-created loops (issue #627): every run is admitted against the creating token's
// limits, caps and budget, and carries its marking, exactly like a hand launch.

fn a_token(max_concurrent: Option<u32>, repos: Vec<&str>) -> crate::api_tokens::NewToken {
    crate::api_tokens::NewToken {
        name: "cron".into(),
        scope: "launch".into(),
        orgs: Vec::new(),
        repos: repos.into_iter().map(str::to_string).collect(),
        max_concurrent,
        budget_usd_per_day: None,
    }
}

/// A test app whose config dir exists (a token creation writes through to it). The refusals
/// below all fire before `create` needs an agent module; only the launch that succeeds uses
/// the full fixture.
fn app_with_config(root: &FsPath) -> Shared {
    std::fs::create_dir_all(root.join("config")).unwrap();
    crate::tests::test_app(root)
}

/// The smallest install `create` insists on, as in sessions' tests; the boot task a created
/// colony spawns is never polled, so nothing reaches a microVM.
fn app_that_can_create(root: &FsPath) -> Shared {
    std::fs::create_dir_all(root.join("config")).unwrap();
    crate::sessions::tests::app_that_can_create(root)
}

#[tokio::test]
async fn a_token_loop_launches_its_colony_under_its_token() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-token-{}", short_id()));
    let app = app_that_can_create(&root);
    let made = app.api_tokens.create(a_token(None, vec!["acme/app"])).await.unwrap();
    let mut l = a_loop(Cadence::Interval { minutes: 60 });
    l.repo = "acme/app".into();
    l.created_by_token = Some(made.meta.id.clone());
    app.loops.loops.write().await.push(l);
    let now = utc(2026, 9, 24, 9, 0);
    let session = launch(&app, &app.loops.get("loop_a").await.unwrap(), now).await.unwrap();
    assert_eq!(
        session.launched_by_token.as_deref(),
        Some(made.meta.id.as_str()),
        "the colony knows the token its loop runs under"
    );
    assert_eq!(session.origin.as_deref(), Some("loop:loop_a"));
    let l = app.loops.get("loop_a").await.unwrap();
    assert_eq!(
        (l.runs, l.last_run.as_ref().map(|r| r.session.as_str())),
        (1, Some(session.id.as_str())),
        "the run is booked like any other"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_token_loop_s_run_is_refused_at_its_token_s_concurrency_cap() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-cap-{}", short_id()));
    let app = app_with_config(&root);
    let made = app.api_tokens.create(a_token(Some(1), vec!["acme/app"])).await.unwrap();
    let mut l = a_loop(Cadence::Interval { minutes: 60 });
    l.repo = "acme/app".into();
    l.created_by_token = Some(made.meta.id.clone());
    app.loops.loops.write().await.push(l);
    // Another colony of the same token holds the one place — it is not the loop's own run, so
    // the one-run-at-a-time skip does not fire first.
    let mut holder = colony("acme", SessionStatus::Running);
    holder.id = "holder".into();
    holder.repo = "acme/app".into();
    holder.launched_by_token = Some(made.meta.id.clone());
    app.sessions.write().await.push(holder);
    let now = utc(2026, 9, 24, 9, 0);
    fire_due(&app, now).await;
    let l = app.loops.get("loop_a").await.unwrap();
    assert!(l.last_run.is_none() && l.runs == 0, "nothing was launched");
    assert!(
        !app.sessions
            .read()
            .await
            .iter()
            .any(|s| s.origin.as_deref() == Some("loop:loop_a")),
        "no colony was started"
    );
    let note = l.last_note.unwrap();
    assert!(note.contains("concurrency cap"), "the tick's note is the refusal: {note}");
    assert_eq!(
        l.next_run_at,
        Some(utc(2026, 9, 24, 10, 0)),
        "the loop tries again at its next slot"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_loop_run_outside_its_token_s_repo_limits_is_refused_like_a_launch() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-limits-{}", short_id()));
    let app = app_with_config(&root);
    // The token's reach moved (or the owner moved the loop): the run refuses at the same gate
    // a hand launch would, and the note says so.
    let made = app.api_tokens.create(a_token(None, vec!["acme/api"])).await.unwrap();
    let mut l = a_loop(Cadence::Interval { minutes: 60 });
    l.created_by_token = Some(made.meta.id.clone());
    app.loops.loops.write().await.push(l);
    let now = utc(2026, 9, 24, 9, 0);
    fire_due(&app, now).await;
    let l = app.loops.get("loop_a").await.unwrap();
    let note = l.last_note.unwrap();
    assert!(note.contains("do not include acme/web"), "{note}");
    assert_eq!(
        (l.runs, l.enabled),
        (0, true),
        "refused, not ended: the limits may widen again"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn revoking_the_token_ends_its_loop() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-revoke-{}", short_id()));
    let app = app_with_config(&root);
    let made = app.api_tokens.create(a_token(None, vec![])).await.unwrap();
    let mut l = a_loop(Cadence::Interval { minutes: 60 });
    l.created_by_token = Some(made.meta.id.clone());
    app.loops.loops.write().await.push(l);
    assert!(app.api_tokens.revoke(&made.meta.id).await.is_some());
    let now = utc(2026, 9, 24, 9, 0);
    fire_due(&app, now).await;
    let l = app.loops.get("loop_a").await.unwrap();
    assert!(!l.enabled && l.next_run_at.is_none(), "the scheduler never picks it up again");
    assert_eq!(l.ended_reason.as_deref(), Some("its API token was revoked"));
    assert_eq!(l.runs, 0, "nothing was launched");
    // The end survives a later tick: the loop is no longer due, and its record is untouched.
    fire_due(&app, now).await;
    let l = app.loops.get("loop_a").await.unwrap();
    assert_eq!(l.last_note.as_deref(), Some("its API token was revoked"));
    let _ = std::fs::remove_dir_all(root);
}

// -- Re-running a run that failed for an infrastructure reason (issue #881) ---------------
/// The app a re-run test starts from: a loop of `kind` whose last run is a colony that ended
/// `Failed` with `class`, started `age_minutes` before `now`, and the loop's own retry window.
/// The loop is enabled and not due, so only the re-run pass can touch it.
async fn app_with_failed_last_run(
    root: &FsPath,
    kind: LoopKind,
    class: FailureClass,
    age_minutes: i64,
    window: Option<u32>,
) -> (Shared, DateTime<Utc>) {
    let app = app_that_can_create(root);
    let now = utc(2026, 9, 24, 9, 0);
    let mut failed = colony("acme", SessionStatus::Failed);
    failed.id = "run1".into();
    failed.repo = "acme/app".into();
    failed.origin = Some("loop:loop_a".into());
    failed.failure_class = Some(class);
    app.sessions.write().await.push(failed);
    let mut l = a_loop(Cadence::Interval { minutes: 60 });
    l.kind = kind;
    l.repo = "acme/app".into();
    l.retry_failed_runs = window;
    l.runs = 1;
    l.last_run = Some(LastRun {
        session: "run1".into(),
        at: now - ChronoDuration::minutes(age_minutes),
        retried: false,
        outcome: None,
    });
    l.next_run_at = Some(now + ChronoDuration::minutes(30));
    app.loops.loops.write().await.push(l);
    (app, now)
}

#[tokio::test]
async fn a_run_that_failed_for_an_infrastructure_reason_is_re_run_once() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-rerun-{}", short_id()));
    let (app, now) = app_with_failed_last_run(&root, LoopKind::Colony, FailureClass::TransientInfra, 5, None).await;
    fire_due(&app, now).await;
    let l = app.loops.get("loop_a").await.unwrap();
    let last = l.last_run.clone().expect("a run");
    assert!(last.retried, "the run is booked as re-run");
    assert_ne!(last.session, "run1", "a fresh colony");
    assert_eq!(
        last.at,
        now - ChronoDuration::minutes(5),
        "the original time is kept, so the schedule does not move"
    );
    assert_eq!(l.runs, 1, "a re-run is not a new run");
    assert_eq!(
        l.next_run_at,
        Some(now + ChronoDuration::minutes(30)),
        "the regular schedule is untouched"
    );
    let fresh = app.session(&last.session).await.expect("the re-run colony exists");
    assert_eq!(fresh.origin.as_deref(), Some("loop:loop_a"));

    // The next tick re-runs nothing: the record is marked, and the fresh run holds the loop.
    let before = app.sessions.read().await.len();
    fire_due(&app, now).await;
    assert_eq!(app.sessions.read().await.len(), before, "no second re-run");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_failed_run_is_not_re_run_outside_its_window_for_a_verdict_switched_off_or_not_a_colony() {
    for (kind, class, age_minutes, window) in [
        (LoopKind::Colony, FailureClass::TransientInfra, 61, None), // past the default hour
        (LoopKind::Colony, FailureClass::Permanent, 5, None),       // a verdict a re-run cannot fix
        (LoopKind::Colony, FailureClass::TransientInfra, 5, Some(0)), // the loop has it off
        (LoopKind::Colony, FailureClass::TransientInfra, 30, Some(15)), // past the loop's window
        (LoopKind::Map, FailureClass::TransientInfra, 5, None),     // a map loop is not `launch`'s
    ] {
        let root = std::env::temp_dir().join(format!("colonizer-loops-norerun-{}", short_id()));
        let (app, now) = app_with_failed_last_run(&root, kind.clone(), class, age_minutes, window).await;
        fire_due(&app, now).await;
        let l = app.loops.get("loop_a").await.unwrap();
        assert_eq!(l.runs, 1);
        let last = l.last_run.clone().expect("a run");
        assert_eq!(
            last.session, "run1",
            "{kind:?}/{class:?}/{age_minutes}/{window:?}: the run is left alone"
        );
        assert!(!last.retried);
        assert_eq!(app.sessions.read().await.len(), 1, "no colony was launched");
        let _ = std::fs::remove_dir_all(root);
    }
}

/// The loop list's last-run outcome is derived on the read, not stored: [`run_outcome`] names
/// the colony's status with a failed run's class on it, `list` reads it from the live store per
/// loop, and it never reaches `loops.json`.
#[tokio::test]
async fn the_loop_list_reports_a_runs_outcome_with_its_class() {
    let root = std::env::temp_dir().join(format!("colonizer-loops-outcome-{}", short_id()));
    let app = app_with_config(&root);
    let mut failed = colony("acme", SessionStatus::Failed);
    failed.id = "run1".into();
    failed.failure_class = Some(FailureClass::TransientInfra);
    let mut opened = colony("acme", SessionStatus::PrOpened);
    opened.id = "run2".into();
    app.sessions.write().await.extend([failed, opened]);
    // The derivation on its own: a class on a failed run, a bare status otherwise, none when the
    // colony is gone.
    let sessions = app.sessions.read().await.clone();
    assert_eq!(run_outcome(&sessions, "run1").as_deref(), Some("failed (transient_infra)"));
    assert_eq!(run_outcome(&sessions, "run2").as_deref(), Some("pr_opened"));
    assert_eq!(run_outcome(&sessions, "gone"), None, "a colony that is gone has no outcome");
    // And `list` derives it per loop.
    let mut l = a_loop(Cadence::Interval { minutes: 60 });
    l.last_run = Some(LastRun {
        session: "run1".into(),
        at: utc(2026, 9, 24, 9, 0),
        retried: false,
        outcome: None,
    });
    let mut never = a_loop(Cadence::Interval { minutes: 60 });
    never.id = "loop_b".into();
    app.loops.loops.write().await.extend([l, never]);
    let Json(loops) = list(State(app.clone()), None).await;
    let by_id = |id: &str| loops.iter().find(|l| l.id == id).unwrap().last_run.clone();
    assert_eq!(by_id("loop_a").unwrap().outcome.as_deref(), Some("failed (transient_infra)"));
    assert!(by_id("loop_b").is_none(), "a loop that has not run has no outcome");
    // And the outcome never reaches loops.json: only `session`, `at` and `retried` persist.
    app.loops.save().await.unwrap();
    let raw: Value = serde_json::from_slice(&std::fs::read(root.join("config/loops.json")).unwrap()).unwrap();
    let saved = raw.as_array().unwrap().iter().find(|x| x["id"] == "loop_a").unwrap();
    assert!(
        saved["last_run"].get("outcome").is_none(),
        "the outcome is not persisted: {saved}"
    );
    let _ = std::fs::remove_dir_all(root);
}
