use super::*;
use std::sync::Mutex;

/// A fake GitHub: one issue's comments and labels, every call recorded, and an error to fail
/// writes with.
#[derive(Default)]
struct FakeGh {
    comments: Mutex<Vec<(u64, String)>>,
    labels: Mutex<Vec<String>>,
    calls: Mutex<Vec<String>>,
    fail_writes: Mutex<Option<String>>,
    fail_labels: Mutex<Option<String>>,
}

impl FakeGh {
    fn count(&self, prefix: &str) -> usize {
        self.calls.lock().unwrap().iter().filter(|c| c.starts_with(prefix)).count()
    }

    fn call(&self, what: String) {
        self.calls.lock().unwrap().push(what);
    }

    fn write_error(&self) -> Result<(), String> {
        self.fail_writes.lock().unwrap().clone().map_or(Ok(()), Err)
    }

    fn label_error(&self) -> Result<(), String> {
        self.fail_labels.lock().unwrap().clone().map_or(Ok(()), Err)
    }

    fn body(&self) -> String {
        self.comments.lock().unwrap().last().unwrap().1.clone()
    }
}

impl ClaimGh for FakeGh {
    async fn comments(&self, _: &str, _: u64) -> Result<Vec<(u64, String)>, String> {
        self.call("list".into());
        Ok(self.comments.lock().unwrap().clone())
    }

    async fn create_comment(&self, _: &str, _: u64, body: &str) -> Result<u64, String> {
        self.call("post".into());
        self.write_error()?;
        let mut comments = self.comments.lock().unwrap();
        let id = 100 + comments.len() as u64;
        comments.push((id, body.to_string()));
        Ok(id)
    }

    async fn edit_comment(&self, _: &str, id: u64, body: &str) -> Result<(), String> {
        self.call(format!("edit {id}"));
        self.write_error()?;
        let mut comments = self.comments.lock().unwrap();
        let entry = comments.iter_mut().find(|(i, _)| *i == id).ok_or("HTTP 404")?;
        entry.1 = body.to_string();
        Ok(())
    }

    async fn create_label(&self, _: &str, name: &str, color: &str, _: &str) -> Result<(), String> {
        self.call(format!("create-label {name} {color}"));
        self.label_error()
    }

    async fn add_labels(&self, _: &str, _: u64, labels: &[&str]) -> Result<(), String> {
        self.call(format!("add {}", labels.join(",")));
        if labels.len() > 1 {
            self.label_error()?;
        }
        let mut have = self.labels.lock().unwrap();
        for l in labels {
            if !have.iter().any(|h| h == l) {
                have.push(l.to_string());
            }
        }
        Ok(())
    }

    async fn remove_label(&self, _: &str, _: u64, label: &str) -> Result<(), String> {
        self.call(format!("remove {label}"));
        self.labels.lock().unwrap().retain(|l| l != label);
        Ok(())
    }
}

fn host() -> HostTag {
    HostTag::new(Some("Omarchy"), "f8832acd-9abe")
}

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-05T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn session(id: &str, status: SessionStatus) -> Session {
    let mut s = crate::sessions::tests::colony("acme", status);
    s.id = id.into();
    s.repo = "acme/app".into();
    s.issue = Some(7);
    s.branch = format!("colonizer/issue-7-{id}");
    s
}

async fn claimed(gh: &FakeGh, live: &mut Live, s: &Session) -> Tracked {
    publish_with(gh, live, "acme/app", view_of(s, None), &host(), true, t0())
        .await
        .expect("claimed")
}

#[test]
fn the_host_label_is_a_stable_slug_with_a_fixed_colour() {
    assert_eq!(host().label, "colonizer:host:omarchy");
    assert_eq!(host().marker, "Omarchy (f8832acd-9abe)");
    assert_eq!(host_slug("My Box.local"), "my-box-local");
    assert_eq!(host_slug("  --  "), "host");
    assert_eq!(HostTag::new(None, "f8832acd-9abe").label, "colonizer:host:f8832acd");
    assert_eq!(host_color("omarchy"), host_color("omarchy"));
}

#[test]
fn the_comment_shows_status_and_no_more() {
    let mut s = session("abc", SessionStatus::PrOpened);
    s.pr_url = Some("https://github.com/acme/app/pull/12".into());
    let body = render(&view_of(&s, Some("old".into())), &host(), t0());
    for want in [
        "`abc`",
        "Omarchy (f8832acd-9abe)",
        "pull request #12 opened",
        "colonizer/issue-7-abc",
        "/pull/12",
        "`old`",
        "2026-10-05 12:00 UTC",
    ] {
        assert!(body.contains(want), "{want} missing from {body}");
    }
    // The marker still parses for the duplicate guard.
    let claim = parse_claim(&body).unwrap();
    assert_eq!((claim.colony.as_str(), claim.issue), ("abc", 7));
    assert_eq!(
        status_of(SessionStatus::Failed, None),
        ("released (colony failed)".into(), true)
    );
    assert!(!status_of(SessionStatus::WaitingForAnswer, None).1);
}

#[tokio::test]
async fn the_comment_is_created_once_then_edited() {
    let gh = FakeGh::default();
    let mut live = Live::default();
    let s = session("abc", SessionStatus::Queued);
    let tracked = claimed(&gh, &mut live, &s).await;
    assert_eq!(gh.count("post"), 1);
    assert_eq!(
        gh.labels.lock().unwrap().clone(),
        vec![CLAIM_LABEL.to_string(), "colonizer:host:omarchy".into()]
    );

    let later = t0() + Duration::minutes(3);
    tick_with(
        &gh,
        &mut live,
        &[session("abc", SessionStatus::Running)],
        |_| true,
        &host(),
        later,
    )
    .await;
    assert_eq!(gh.count(&format!("edit {}", tracked.comment_id)), 1);
    assert!(gh.body().contains("Status: running"));
    // Nothing new to say: no request at all.
    tick_with(
        &gh,
        &mut live,
        &[session("abc", SessionStatus::Running)],
        |_| true,
        &host(),
        later + Duration::minutes(5),
    )
    .await;
    assert_eq!(gh.count("edit"), 1);
    assert_eq!(gh.count("post"), 1, "never a second comment");
    assert_eq!(gh.comments.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn edits_are_debounced_but_terminal_states_always_land() {
    let gh = FakeGh::default();
    let mut live = Live::default();
    claimed(&gh, &mut live, &session("abc", SessionStatus::Queued)).await;
    let soon = t0() + Duration::seconds(30);
    tick_with(
        &gh,
        &mut live,
        &[session("abc", SessionStatus::Running)],
        |_| true,
        &host(),
        soon,
    )
    .await;
    assert_eq!(gh.count("edit"), 0, "within two minutes of the last edit");
    let mut merged = session("abc", SessionStatus::Merged);
    merged.pr_url = Some("https://github.com/acme/app/pull/12".into());
    tick_with(
        &gh,
        &mut live,
        &[merged.clone()],
        |_| true,
        &host(),
        soon + Duration::seconds(1),
    )
    .await;
    assert_eq!(gh.count("edit"), 1, "a terminal state is never held back");
    assert!(gh.body().contains("pull request #12 merged"));
    assert!(live.issues.is_empty(), "a final state ends the tracking");
}

#[tokio::test]
async fn a_restart_finds_the_marker_instead_of_posting_again() {
    let gh = FakeGh::default();
    let mut live = Live::default();
    let tracked = claimed(&gh, &mut live, &session("abc", SessionStatus::Queued)).await;
    // The mothership restarts: memory is gone, the comment is not.
    let mut fresh = Live::default();
    let running = session("abc", SessionStatus::Running);
    tick_with(&gh, &mut fresh, std::slice::from_ref(&running), |_| true, &host(), t0()).await;
    assert_eq!(gh.count(&format!("edit {}", tracked.comment_id)), 1);
    assert_eq!(gh.count("post"), 1);
    // Looked up once only.
    tick_with(&gh, &mut fresh, &[running], |_| true, &host(), t0() + Duration::minutes(10)).await;
    assert_eq!(gh.count("list"), 2);
}

#[tokio::test]
async fn a_retry_edits_the_same_comment_and_names_the_colony_before() {
    let gh = FakeGh::default();
    let mut live = Live::default();
    let first = claimed(&gh, &mut live, &session("abc", SessionStatus::Queued)).await;
    release_with(
        &gh,
        &mut live,
        Release {
            repo: "acme/app",
            issue: 7,
            colony: "abc",
            reason: "released (colony failed)",
        },
        &host(),
        t0(),
    )
    .await;
    assert!(gh.body().contains("released (colony failed)"));
    assert!(gh.labels.lock().unwrap().is_empty(), "both labels come off");
    let second = claimed(&gh, &mut live, &session("def", SessionStatus::Queued)).await;
    assert_eq!(first.comment_id, second.comment_id);
    assert_eq!(gh.comments.lock().unwrap().len(), 1);
    assert!(gh.body().contains("Previous colony: `abc`"));
    assert_eq!(gh.labels.lock().unwrap().len(), 2);
    // After a restart the previous colony is read back from the comment itself.
    let mut fresh = Live::default();
    tick_with(
        &gh,
        &mut fresh,
        &[session("def", SessionStatus::Running)],
        |_| true,
        &host(),
        t0(),
    )
    .await;
    assert!(gh.body().contains("Status: running") && gh.body().contains("Previous colony: `abc`"));
}

#[tokio::test]
async fn a_release_never_touches_a_successors_claim() {
    let gh = FakeGh::default();
    let mut live = Live::default();
    claimed(&gh, &mut live, &session("def", SessionStatus::Running)).await;
    release_with(
        &gh,
        &mut live,
        Release {
            repo: "acme/app",
            issue: 7,
            colony: "abc",
            reason: "released (colony stopped)",
        },
        &host(),
        t0(),
    )
    .await;
    assert_eq!(gh.count("remove"), 0);
    assert!(gh.body().contains("`def`") && !gh.body().contains("released"));
}

#[tokio::test]
async fn a_missing_label_permission_degrades_to_the_comment() {
    let gh = FakeGh::default();
    *gh.fail_labels.lock().unwrap() = Some("gh: Resource not accessible by integration (HTTP 403)".into());
    let mut live = Live::default();
    claimed(&gh, &mut live, &session("abc", SessionStatus::Queued)).await;
    assert_eq!(gh.labels.lock().unwrap().clone(), vec![CLAIM_LABEL.to_string()]);
    assert!(!live.paused(t0()), "a permission 403 on a label is no push-back");
}

#[tokio::test]
async fn a_push_back_pauses_every_edit_and_doubles() {
    let gh = FakeGh::default();
    let mut live = Live::default();
    claimed(&gh, &mut live, &session("abc", SessionStatus::Queued)).await;
    *gh.fail_writes.lock().unwrap() = Some("gh: You have exceeded a secondary rate limit (HTTP 403)".into());
    let later = t0() + Duration::minutes(3);
    tick_with(
        &gh,
        &mut live,
        &[session("abc", SessionStatus::Running)],
        |_| true,
        &host(),
        later,
    )
    .await;
    assert_eq!(live.paused_until, Some(later + Duration::minutes(15)));
    // Paused: not even a terminal state is tried.
    let merged = session("abc", SessionStatus::Merged);
    tick_with(
        &gh,
        &mut live,
        std::slice::from_ref(&merged),
        |_| true,
        &host(),
        later + Duration::minutes(5),
    )
    .await;
    assert_eq!(gh.count("edit"), 1);
    let resumed = later + Duration::minutes(16);
    tick_with(&gh, &mut live, std::slice::from_ref(&merged), |_| true, &host(), resumed).await;
    assert_eq!(
        live.paused_until,
        Some(resumed + Duration::minutes(30)),
        "a second push-back doubles"
    );
    *gh.fail_writes.lock().unwrap() = None;
    let later_still = resumed + Duration::minutes(31);
    tick_with(&gh, &mut live, &[merged], |_| true, &host(), later_still).await;
    assert_eq!(live.strikes, 0);
    assert!(gh.body().contains("merged"));
}

#[tokio::test]
async fn an_org_that_opted_out_gets_no_host_label_and_no_status_edits() {
    let gh = FakeGh::default();
    let mut live = Live::default();
    publish_with(
        &gh,
        &mut live,
        "acme/app",
        view_of(&session("abc", SessionStatus::Queued), None),
        &host(),
        false,
        t0(),
    )
    .await
    .unwrap();
    assert_eq!(gh.labels.lock().unwrap().clone(), vec![CLAIM_LABEL.to_string()]);
    tick_with(
        &gh,
        &mut live,
        &[session("abc", SessionStatus::Running)],
        |_| false,
        &host(),
        t0() + Duration::minutes(5),
    )
    .await;
    assert_eq!(gh.count("edit"), 0);
}

#[tokio::test]
async fn the_kill_switch_stops_claims_entirely() {
    let root = crate::tests::temp_root();
    let app = crate::tests::test_app(&root);
    let _blocked = crate::authority::test_block_external_writes();
    // With writes blocked, neither returns having called `gh` (which the test app could not run).
    crate::claims::publish_claim(&app, "acme/app", 7, "abc").await;
    crate::claims::release_claim(&app, "acme/app", 7, "abc", "released (colony stopped)").await;
    assert!(LIVE.lock().await.get(&app.cfg.config_dir).is_none());
}
