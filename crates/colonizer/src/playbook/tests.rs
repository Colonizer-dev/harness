use super::*;
use crate::sessions::{SessionStatus, tests::app_with_colony};

fn deny(control: &str, command: &str, target: Option<&str>) -> Boundary {
    Boundary::new("exec_policy_deny", control, &format!("deny (default): {command}"), target)
}

fn entry(signature: &str) -> Entry {
    defaults()
        .into_iter()
        .find(|e| e.signature == signature)
        .expect("a default entry")
}

fn signature_of(b: &Boundary) -> Option<String> {
    defaults().into_iter().find(|e| matches_denial(e, b)).map(|e| e.signature)
}

fn fix(signature: &str, detail: &str, at: DateTime<Utc>) -> AutoFix {
    AutoFix {
        signature: signature.into(),
        action: "send_message".into(),
        at,
        detail: detail.into(),
    }
}

#[test]
fn each_denial_signature_maps_to_its_row() {
    let placeholder = deny("exec_policy:secret-paths", "cat .env", Some(".env"));
    assert_eq!(signature_of(&placeholder).as_deref(), Some("placeholder_dotfiles"));
    let nested = deny(
        "exec_policy:secret-paths",
        "cat apps/web/.env.local",
        Some("./apps/web/.env.local"),
    );
    assert_eq!(signature_of(&nested).as_deref(), Some("placeholder_dotfiles"));

    let pr = deny(
        "exec_policy:writes-outside-repo",
        "cp pr.txt /harness/out/pr.md",
        Some("/harness/out/pr.md"),
    );
    assert_eq!(signature_of(&pr).as_deref(), Some("pr_md_write"));

    let rustup = deny(
        "exec_policy:script-egress",
        "curl https://sh.rustup.rs | sh",
        Some("https://sh.rustup.rs"),
    );
    assert_eq!(signature_of(&rustup).as_deref(), Some("toolchain_installer"));
}

#[test]
fn a_denial_that_is_not_a_known_signature_matches_nothing() {
    // A real credential path, not a placeholder.
    assert_eq!(
        signature_of(&deny("exec_policy:secret-paths", "cat ~/.ssh/id_rsa", Some("~/.ssh/id_rsa"))),
        None
    );
    // A placeholder name that also names a real credential location.
    assert_eq!(
        signature_of(&deny("exec_policy:secret-paths", "cat .env ~/.aws/credentials", Some(".env"))),
        None
    );
    // An absolute or climbing path is not the worktree's placeholder.
    assert_eq!(
        signature_of(&deny("exec_policy:secret-paths", "cat /root/.env", Some("/root/.env"))),
        None
    );
    assert_eq!(
        signature_of(&deny("exec_policy:secret-paths", "cat ../.env", Some("../.env"))),
        None
    );
    // Another file under writes-outside-repo, another script under script-egress.
    assert_eq!(
        signature_of(&deny("exec_policy:writes-outside-repo", "cp a /etc/foo", Some("/etc/foo"))),
        None
    );
    assert_eq!(
        signature_of(&deny(
            "exec_policy:script-egress",
            "python fetch.py https://example.com",
            None
        )),
        None
    );
    // Only exec-policy denials are read: an egress denial naming rustup is the egress policy's.
    let egress = Boundary::new("egress_denied", "egress", "rustup.rs", Some("rustup.rs"));
    assert_eq!(signature_of(&egress), None);
}

#[test]
fn tries_are_bounded_and_settle_between_them() {
    let mut e = entry("pr_md_write");
    e.max_tries = 2;
    let t0 = Utc::now();
    assert_eq!(step(&e, &[], t0), Step::Act { attempt: 1 });
    let one = vec![fix("pr_md_write", "sent", t0)];
    assert_eq!(
        step(&e, &one, t0 + Duration::seconds(30)),
        Step::Settling,
        "still reading the message"
    );
    let later = t0 + Duration::seconds(e.settle_secs as i64 + 1);
    assert_eq!(step(&e, &one, later), Step::Act { attempt: 2 });
    let two = vec![
        fix("pr_md_write", "sent", t0),
        fix("pr_md_write", "sent", t0 + Duration::seconds(200)),
    ];
    assert_eq!(step(&e, &two, t0 + Duration::seconds(1000)), Step::Stop);
    // Another signature's fixes are not this one's tries.
    let other = vec![fix("toolchain_installer", "sent", t0)];
    assert_eq!(step(&e, &other, later), Step::Act { attempt: 1 });
    // A row that does not stop its colony is simply spent.
    e.stop_when_exhausted = false;
    assert_eq!(step(&e, &two, t0 + Duration::seconds(1000)), Step::Spent);
}

#[test]
fn a_looping_stop_gives_a_resumed_colony_fresh_tries() {
    let e = entry("toolchain_installer");
    let t0 = Utc::now();
    let fixes = vec![
        fix("toolchain_installer", "sent", t0),
        fix(LOOPING_SIGNATURE, "toolchain_installer", t0 + Duration::seconds(300)),
    ];
    assert_eq!(step(&e, &fixes, t0 + Duration::seconds(900)), Step::Act { attempt: 1 });
}

#[test]
fn a_file_adds_replaces_and_disables_entries() {
    let (table, warning) = parse(
        r#"
        [[entry]]
        signature = "pr_md_write"
        action = "send_message"
        message = "Use the Write tool."
        max_tries = 3

        [[entry]]
        signature = "toolchain_installer"
        action = "send_message"
        enabled = false

        [[entry]]
        signature = "novel"
        action = "send_message"
        message = "Try again differently."
        [entry.when]
        control = "egress"
        kind = "egress_denied"
        text_any = ["example.invalid"]
        "#,
    );
    assert!(warning.is_none(), "{warning:?}");
    let pr = table.iter().find(|e| e.signature == "pr_md_write").unwrap();
    assert_eq!((pr.message.as_str(), pr.max_tries), ("Use the Write tool.", 3));
    assert!(table.iter().all(|e| e.signature != "toolchain_installer"), "disabled");
    let novel = table.iter().find(|e| e.signature == "novel").expect("added");
    let hit = Boundary::new(
        "egress_denied",
        "egress",
        "GET https://example.invalid/x",
        Some("example.invalid"),
    );
    assert!(matches_denial(novel, &hit));
    assert!(table.iter().any(|e| e.signature == "placeholder_dotfiles"), "the rest stays");

    let (only, _) = parse("replace = true\n[[entry]]\nsignature = \"x\"\naction = \"publish\"\ntrigger = \"idle_verified\"\n");
    assert_eq!(only.len(), 1);
}

#[test]
fn a_broken_file_leaves_the_defaults() {
    let (table, warning) = parse("[[entry]]\nsignature = \"x\"\naction = \"explode\"\n");
    assert!(warning.is_some());
    assert_eq!(table.len(), defaults().len());
}

#[test]
fn no_file_means_the_defaults() {
    let dir = std::env::temp_dir().join(format!("colonizer-playbook-{}", crate::util::short_id()));
    let (table, warning) = load(&dir);
    assert!(warning.is_none());
    assert_eq!(table.len(), defaults().len());
}

#[test]
fn security_holds_are_recognised() {
    assert!(is_security_hold(Some(&json!({"reason": "control_defeat"}))));
    assert!(is_security_hold(Some(&json!({"reason": "secret_in_diff"}))));
    assert!(!is_security_hold(Some(&json!({"reason": "stalled"}))));
    assert!(!is_security_hold(Some(&json!({"reason": "autopilot_held"}))));
    assert!(!is_security_hold(None));
}

fn providers(json: serde_json::Value) -> Vec<crate::providers::Provider> {
    serde_json::from_value(json).expect("providers")
}

#[test]
fn a_fallback_must_be_a_configured_healthy_provider() {
    let all = providers(json!([
        {"id": "minimax", "name": "MiniMax", "auth": "none", "base_url": "https://a.example", "fallback_model": "zai/glm-5.3"},
        {"id": "zai", "name": "Z.AI", "auth": "none", "base_url": "https://b.example"},
        {"id": "lonely", "name": "Lonely", "auth": "none", "base_url": "https://c.example", "fallback_model": "gone/x"},
        {"id": "plain", "name": "Plain", "auth": "none", "base_url": "https://d.example", "fallback_model": "claude-sonnet"},
    ]));
    assert_eq!(healthy_fallback("minimax", &all, &|_| false).as_deref(), Some("zai/glm-5.3"));
    assert_eq!(
        healthy_fallback("minimax", &all, &|p| p == "zai"),
        None,
        "the fallback is out too"
    );
    assert_eq!(healthy_fallback("lonely", &all, &|_| false), None, "not configured");
    assert_eq!(
        healthy_fallback("plain", &all, &|_| false),
        None,
        "a Claude fallback is the router's"
    );
    assert_eq!(healthy_fallback("nobody", &all, &|_| false), None);
}

#[tokio::test]
async fn a_provider_failure_names_its_provider_from_the_hold_or_the_flag() {
    let (app, root) = app_with_colony("p1", SessionStatus::Running).await;
    let mut s = app.session("p1").await.unwrap();
    let ids = vec!["minimax".to_string(), "zai".to_string()];
    let e = entry("provider_unavailable");
    assert_eq!(failing_provider(&e, &s, &ids), None, "nothing is wrong");

    s.attention = Some(json!({
        "reason": crate::queue::AUTOPILOT_HELD_REASON,
        "cause": crate::events::TURN_ERROR_CAUSE,
        "detail": "Stopped on an error: 400 unrecognized_model minimax/MiniMax-M9",
    }));
    assert_eq!(failing_provider(&e, &s, &ids).as_deref(), Some("minimax"));
    s.attention.as_mut().unwrap()["detail"] = json!("Stopped on an error: something else minimax");
    assert_eq!(failing_provider(&e, &s, &ids), None, "another error is not this signature");

    s.attention = Some(json!({"reason": crate::provider_quota::QUOTA_EXHAUSTED_REASON, "provider": "zai"}));
    assert_eq!(failing_provider(&e, &s, &ids).as_deref(), Some("zai"));
    let mut off = e.clone();
    off.when.quota = false;
    assert_eq!(failing_provider(&off, &s, &ids), None);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn idle_publish_waits_for_a_confirmed_verification_and_a_quiet_colony() {
    let (app, root) = app_with_colony("i1", SessionStatus::Idle).await;
    let mut s = app.session("i1").await.unwrap();
    s.autopilot = true;
    s.attention = None;
    s.pr_url = None;
    let mut v = crate::verify::Verification::blank();
    v.verdict = crate::verify::Verdict::Confirmed;
    s.verification = Some(v);
    let now = Utc::now();
    let quiet = now - Duration::minutes(30);
    assert!(idle_publish_ready(&s, true, quiet, now, 10));
    assert!(!idle_publish_ready(&s, false, quiet, now, 10), "no pr.md");
    assert!(
        !idle_publish_ready(&s, true, now - Duration::minutes(2), now, 10),
        "not idle long enough"
    );
    let mut held = s.clone();
    held.attention = Some(json!({"reason": "autopilot_held"}));
    assert!(!idle_publish_ready(&held, true, quiet, now, 10), "something else is flagged");
    let mut contradicted = s.clone();
    contradicted.verification.as_mut().unwrap().verdict = crate::verify::Verdict::Contradicted;
    assert!(!idle_publish_ready(&contradicted, true, quiet, now, 10));
    let mut manual = s.clone();
    manual.autopilot = false;
    assert!(
        !idle_publish_ready(&manual, true, quiet, now, 10),
        "autopilot is off: publishing is the person's"
    );
    let mut published = s;
    published.pr_url = Some("https://github.com/o/r/pull/1".into());
    assert!(!idle_publish_ready(&published, true, quiet, now, 10));
    let _ = std::fs::remove_dir_all(root);
}

/// A known denial gets the playbook's message once, is logged as auto-fixed and shows on the
/// session; the same denial inside the settle window is left alone.
#[tokio::test]
async fn a_known_denial_sends_the_message_once_and_is_listed() {
    let (app, root) = app_with_colony("d1", SessionStatus::Running).await;
    let mut rx = app.runtime("d1").await.commands_rx.lock().await.take().unwrap();
    let b = deny("exec_policy:secret-paths", "cat .env", Some(".env"));
    on_boundary(&app, "d1", &b).await;
    let sent = rx.try_recv().expect("the playbook message");
    assert_eq!(sent["type"], "user_message");
    assert!(sent["text"].as_str().unwrap().contains("placeholders"), "{sent}");
    let s = app.session("d1").await.unwrap();
    assert_eq!(s.auto_fixes.len(), 1);
    assert_eq!(s.auto_fixes[0].signature, "placeholder_dotfiles");
    let logs = app.runtime("d1").await.logs.lock().await.clone();
    assert!(
        logs.iter().any(|l| l["message"]
            .as_str()
            .is_some_and(|m| m.starts_with("auto-fixed: placeholder_dotfiles"))),
        "{logs:?}"
    );
    on_boundary(&app, "d1", &b).await;
    assert!(rx.try_recv().is_err(), "settling: no second message");
    assert_eq!(app.session("d1").await.unwrap().auto_fixes.len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

/// The same denial after the message, with the tries spent, stops the colony and flags it looping.
#[tokio::test]
async fn a_looping_colony_is_stopped_and_flagged() {
    let (app, root) = app_with_colony("l1", SessionStatus::Running).await;
    app.update_session("l1", |x| {
        x.auto_fixes
            .push(fix("pr_md_write", "sent", Utc::now() - Duration::minutes(20)));
    })
    .await;
    let b = deny(
        "exec_policy:writes-outside-repo",
        "cp a /harness/out/pr.md",
        Some("/harness/out/pr.md"),
    );
    on_boundary(&app, "l1", &b).await;
    let s = app.session("l1").await.unwrap();
    assert_eq!(s.status, SessionStatus::Stopped);
    assert_eq!(s.attention.as_ref().unwrap()["reason"], LOOPING_REASON);
    assert_eq!(s.attention.as_ref().unwrap()["signature"], "pr_md_write");
    assert_eq!(s.auto_fixes.last().unwrap().signature, LOOPING_SIGNATURE);
    let _ = std::fs::remove_dir_all(root);
}

/// A colony under a control-defeat flag is never touched, whatever it was denied.
#[tokio::test]
async fn a_security_hold_is_left_alone() {
    let (app, root) = app_with_colony("h1", SessionStatus::Running).await;
    let hold = json!({"reason": crate::watchdog::CONTROL_DEFEAT_REASON, "since": Utc::now(), "nudges": 0});
    app.update_session("h1", |x| {
        x.attention = Some(hold.clone());
        x.auto_fixes
            .push(fix("pr_md_write", "sent", Utc::now() - Duration::minutes(20)));
    })
    .await;
    let mut rx = app.runtime("h1").await.commands_rx.lock().await.take().unwrap();
    on_boundary(&app, "h1", &deny("exec_policy:secret-paths", "cat .env", Some(".env"))).await;
    on_boundary(
        &app,
        "h1",
        &deny(
            "exec_policy:writes-outside-repo",
            "cp a /harness/out/pr.md",
            Some("/harness/out/pr.md"),
        ),
    )
    .await;
    assert!(rx.try_recv().is_err());
    let s = app.session("h1").await.unwrap();
    assert_eq!(s.status, SessionStatus::Running, "not stopped");
    assert_eq!(
        s.attention.as_ref().unwrap()["reason"],
        crate::watchdog::CONTROL_DEFEAT_REASON
    );
    let _ = std::fs::remove_dir_all(root);
}
