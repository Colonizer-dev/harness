use super::*;
use crate::sessions::{SessionStatus, tests::colony};
use axum::{body::Body, extract::Request};
use tower::ServiceExt as _;

fn at(minutes: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-01T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + Duration::minutes(minutes)
}

// ---------------------------------------------------------------------------------------------
// The classifier, on what GitHub, gh and git really print.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_suspension_is_told_apart_from_a_refused_credential() {
    for text in [
        "gh: Sorry. Your account was suspended. (HTTP 403)",
        "HTTP/2.0 403 Forbidden\r\nContent-Type: application/json; charset=utf-8\r\n\r\n\
         {\"message\":\"Sorry. Your account was suspended.\",\"documentation_url\":\"https://docs.github.com/rest\",\"status\":\"403\"}",
        "remote: Your account is suspended. Please visit https://support.github.com for more information.\n\
         fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: 403",
    ] {
        assert_eq!(classify(text), Failure::Suspended, "{text}");
    }
}

#[test]
fn a_dead_token_reads_as_revoked() {
    for text in [
        "gh: Bad credentials (HTTP 401)",
        "HTTP/2.0 401 Unauthorized\r\n\r\n{\"message\":\"Bad credentials\",\"documentation_url\":\"https://docs.github.com/rest\",\"status\":\"401\"}",
        "remote: Invalid username or token. Password authentication is not supported for Git operations.\n\
         fatal: Authentication failed for 'https://github.com/o/r.git/'",
        "{\"message\":\"Requires authentication\",\"status\":\"401\"}",
    ] {
        assert_eq!(classify(text), Failure::TokenRevoked, "{text}");
    }
}

#[test]
fn a_named_scope_is_a_missing_scope_not_a_dead_token() {
    for text in [
        "error: your authentication token is missing required scopes [workflow]\nTo request it, run:  gh auth refresh -s workflow",
        "GraphQL: Your token has not been granted the required scopes to execute this query. The 'login' field requires one \
         of the following scopes: ['read:org'], but your token has only been granted the: ['repo'] scopes.",
        " ! [remote rejected] colonizer/x -> colonizer/x (refusing to allow an OAuth App to create or update workflow \
         `.github/workflows/ci.yml` without `workflow` scope)",
        "gh: This API operation needs the \"admin:org\" scope. (HTTP 403)",
    ] {
        assert_eq!(classify(text), Failure::MissingScope, "{text}");
    }
}

#[test]
fn secondary_limits_are_recognised_by_their_words_or_retry_after() {
    for text in [
        "gh: You have exceeded a secondary rate limit. Please wait a few minutes before you try again. If you reach out to \
         GitHub Support for help, please include the request ID C0DE:1234. (HTTP 403)",
        "HTTP/2.0 429 Too Many Requests\r\nRetry-After: 60\r\n\r\n{\"message\":\"Too many requests\"}",
        "HTTP/2.0 403 Forbidden\r\nRetry-After: 120\r\n\r\n{}",
        "gh: You have triggered an abuse detection mechanism. Please wait a few minutes before you try again. (HTTP 403)",
    ] {
        assert_eq!(classify(text), Failure::SecondaryRateLimit, "{text}");
    }
}

#[test]
fn blips_are_transient_and_the_rest_is_other() {
    for text in [
        "gh: API rate limit exceeded for user ID 1234. (HTTP 403)",
        "gh: Server Error (HTTP 502)",
        "HTTP/2.0 503 Service Unavailable\r\n\r\n",
        "error connecting to api.github.com\ncheck your internet connection or https://githubstatus.com",
        "fatal: unable to access 'https://github.com/o/r.git/': Could not resolve host: github.com",
    ] {
        assert_eq!(classify(text), Failure::Transient, "{text}");
    }
    for text in [
        "gh: Not Found (HTTP 404)",
        "GraphQL: Could not resolve to a Repository with the name 'o/r'. (repository)",
        "gh: Validation Failed (HTTP 422)",
        "gh: Resource not accessible by integration (HTTP 403)",
        "HTTP/2.0 304 Not Modified\r\n\r\n",
    ] {
        assert_eq!(classify(text), Failure::Other, "{text}");
    }
}

#[test]
fn statuses_are_read_in_every_spelling() {
    assert_eq!(statuses("gh: bad credentials (http 401)"), [401]);
    assert_eq!(statuses("http/2.0 403 forbidden"), [403]);
    assert_eq!(statuses("http/1.1 429 too many"), [429]);
    assert_eq!(statuses("the requested url returned error: 403"), [403]);
    assert_eq!(statuses("{\"status\":\"401\"}"), [401]);
    assert!(statuses("https://github.com/o/r").is_empty());
}

#[test]
fn the_access_wording_still_recognises_a_suspension() {
    assert_eq!(
        crate::github::classify("gh: Sorry. Your account was suspended. (HTTP 403)"),
        Some(crate::github::Denial::Suspended)
    );
    // The pause's own message keeps the suspension wording when it surfaces through `access_error`.
    let open = Open {
        cause: Cause::Suspended,
        since: at(0),
        next_probe: at(30),
        probes: 0,
        detail: String::new(),
    };
    assert_eq!(
        crate::github::classify(&pause_message(&open)),
        Some(crate::github::Denial::Suspended)
    );
    assert!(!crate::github::is_transient(&pause_message(&Open {
        cause: Cause::TokenRevoked,
        ..open
    })));
}

// ---------------------------------------------------------------------------------------------
// The breaker, with the clock passed in.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_suspension_or_a_revoked_token_opens_at_once() {
    let mut b = Breaker::default();
    assert!(b.observe("me", Failure::Suspended, "suspended", at(0)));
    let open = b.open_for("me").unwrap().clone();
    assert_eq!((open.cause, open.since, open.next_probe), (Cause::Suspended, at(0), at(30)));
    assert!(
        !b.observe("me", Failure::Suspended, "again", at(1)),
        "a repeat changes nothing"
    );
    assert!(b.open_for("someone else").is_none(), "per identity");

    let mut b = Breaker::default();
    assert!(b.observe("me", Failure::TokenRevoked, "bad credentials", at(0)));
    assert_eq!(b.open_for("me").unwrap().cause, Cause::TokenRevoked);

    for failure in [Failure::MissingScope, Failure::Transient, Failure::Other] {
        let mut b = Breaker::default();
        assert!(!b.observe("me", failure, "x", at(0)));
        assert!(b.open_for("me").is_none(), "{failure:?} never opens it");
    }
}

#[test]
fn secondary_limits_open_only_at_the_threshold_within_the_window() {
    let mut b = Breaker::default();
    assert!(!b.observe("me", Failure::SecondaryRateLimit, "1", at(0)));
    assert!(!b.observe("me", Failure::SecondaryRateLimit, "2", at(4)));
    // The first hit has left the ten-minute window: two in the window, still closed.
    assert!(!b.observe("me", Failure::SecondaryRateLimit, "3", at(11)));
    assert!(b.open_for("me").is_none());
    assert!(
        b.observe("me", Failure::SecondaryRateLimit, "4", at(12)),
        "three within ten minutes"
    );
    let open = b.open_for("me").unwrap();
    assert_eq!((open.cause, open.next_probe), (Cause::SecondaryRateLimit, at(17)));
    // A suspension found behind the limits takes over.
    assert!(b.observe("me", Failure::Suspended, "suspended", at(13)));
    assert_eq!(b.open_for("me").unwrap().cause, Cause::Suspended);
}

#[test]
fn failed_probes_back_off_for_secondary_limits_and_stay_slow_for_a_suspension() {
    let mut b = Breaker::default();
    for minute in 0..SECONDARY_THRESHOLD as i64 {
        b.observe("me", Failure::SecondaryRateLimit, "limit", at(minute));
    }
    let mut waits = Vec::new();
    for _ in 0..4 {
        let now = b.open_for("me").unwrap().next_probe;
        b.probe_failed(Failure::SecondaryRateLimit, "limit", now);
        waits.push((b.open_for("me").unwrap().next_probe - now).num_minutes());
    }
    assert_eq!(waits, [10, 20, 30, 30]);
    // The probe finds a suspension: the cause sharpens and the wait is the slow one.
    b.probe_failed(Failure::Suspended, "suspended", at(100));
    let open = b.open_for("me").unwrap();
    assert_eq!((open.cause, open.next_probe), (Cause::Suspended, at(130)));
    b.probe_failed(Failure::Transient, "timeout", at(130));
    assert_eq!(b.open_for("me").unwrap().next_probe, at(160));
    assert!(b.close());
    assert!(b.open_for("me").is_none());
}

// ---------------------------------------------------------------------------------------------
// Through the real call sites, against a fake GitHub that counts its calls.
// ---------------------------------------------------------------------------------------------

/// A fake `gh` that logs every call and answers as `mode` says: `suspended` the way GitHub answers a
/// suspended account, `ok` a 200 for `gh api -i user`.
struct FakeGitHub {
    root: PathBuf,
}

impl FakeGitHub {
    fn new(root: &Path) -> (Self, impl Drop + use<>) {
        std::fs::create_dir_all(root).unwrap();
        let script = root.join("gh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\nmode=$(cat '{mode}')\n\
                 if [ \"$mode\" = ok ]; then\n\
                   printf 'HTTP/2.0 200 OK\\r\\nContent-Type: application/json\\r\\n\\r\\n{{\"login\":\"octo\",\"id\":1}}'\n\
                   exit 0\nfi\n\
                 printf 'HTTP/2.0 403 Forbidden\\r\\n\\r\\n{{\"message\":\"Sorry. Your account was suspended.\"}}'\n\
                 echo 'gh: Sorry. Your account was suspended. (HTTP 403)' >&2\nexit 1\n",
                log = root.join("calls.log").display(),
                mode = root.join("mode").display(),
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fake = FakeGitHub {
            root: root.to_path_buf(),
        };
        fake.set("suspended");
        let guard = test_fake_gh(script);
        (fake, guard)
    }

    fn set(&self, mode: &str) {
        std::fs::write(self.root.join("mode"), mode).unwrap();
    }

    fn calls(&self) -> usize {
        std::fs::read_to_string(self.root.join("calls.log"))
            .map(|log| log.lines().count())
            .unwrap_or(0)
    }
}

async fn queued_app(name: &str) -> (PathBuf, Shared) {
    let root = std::env::temp_dir().join(format!("colonizer-breaker-{name}-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    // A token of its own, so the identity is this test's, and no free-space floor holding the queue
    // for a reason this test is not about (as the drain test does).
    crate::util::write_secret(&app.github_token_file(), &format!("token-{name}")).unwrap();
    app.modules
        .write()
        .await
        .sandbox
        .settings
        .insert("min_free_disk".into(), json!("0"));
    let mut waiting = colony("acme", SessionStatus::Queued);
    waiting.id = "waiting".into();
    *app.sessions.write().await = vec![waiting];
    (root, app)
}

#[tokio::test]
async fn a_suspension_opens_the_breaker_and_nothing_else_reaches_github_until_the_probe() {
    let (root, app) = queued_app("suspension").await;
    let (gh, _fake) = FakeGitHub::new(&root.join("fake"));

    // The first refusal goes to GitHub, and is the last call until the probe.
    let first = crate::github::fetch_issue(&app, "acme/repo", 7).await.unwrap_err();
    assert!(format!("{first:#}").contains("suspended"), "{first:#}");
    assert_eq!(gh.calls(), 1);
    let open = paused(&app).expect("the suspension opened the breaker");
    assert_eq!(open.cause, Cause::Suspended);

    // Every path that reaches GitHub is refused without a call: reads, the cached GET, the
    // viewer, git's network commands, and the queue.
    let refused = crate::github::fetch_issue(&app, "acme/repo", 7).await.unwrap_err();
    assert!(format!("{refused:#}").contains("GitHub is paused"), "{refused:#}");
    assert!(crate::github::gh_get(&app, "repos/acme/repo", None).await.is_err());
    assert!(crate::github::default_branch(&app, "acme/repo").await.is_err());
    assert!(crate::github::viewer(&app).await.is_err());
    let git = crate::util::exec(app.git_remote().args(["ls-remote", "https://github.com/acme/repo"])).await;
    assert!(format!("{:#}", git.unwrap_err()).contains("github (paused)"));
    crate::queue::start_queued(&app).await;
    crate::queue::start_queued(&app).await;
    assert_eq!(
        app.session("waiting").await.unwrap().status,
        SessionStatus::Queued,
        "a queued colony keeps its place while GitHub refuses the account"
    );
    assert_eq!(gh.calls(), 1, "nothing called GitHub while the breaker was open");

    // The probe waits its 30 minutes...
    let since = open.since;
    let mut probed = 0;
    assert!(
        !probe_tick_with(&app, since + Duration::minutes(29), || {
            probed += 1;
            async { Ok::<(), String>(()) }
        })
        .await
    );
    assert_eq!(probed, 0, "not due yet");
    // ...then makes its one call, which still finds the suspension...
    assert!(!probe_tick_with(&app, since + Duration::minutes(30), || probe_github(&app)).await);
    assert_eq!(gh.calls(), 2, "one probe, one call");
    assert!(paused(&app).is_some());
    // ...and once GitHub answers, the breaker closes and the queue moves on its next tick.
    gh.set("ok");
    let next = paused(&app).unwrap().next_probe;
    assert!(probe_tick_with(&app, next, || probe_github(&app)).await);
    assert_eq!(gh.calls(), 3);
    assert!(paused(&app).is_none());
    crate::queue::start_queued(&app).await;
    assert_ne!(
        app.session("waiting").await.unwrap().status,
        SessionStatus::Queued,
        "the queue resumes once the account works again"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn secondary_limits_through_the_call_sites_open_only_at_the_threshold() {
    let root = std::env::temp_dir().join(format!("colonizer-breaker-secondary-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    let identity = identity(&app);
    let limit = "gh: You have exceeded a secondary rate limit. Please wait a few minutes before you try again. (HTTP 403)";
    for hit in 0..SECONDARY_THRESHOLD - 1 {
        assert_eq!(
            observe(&app.cfg.config_dir, &identity, limit, at(hit as i64)),
            Failure::SecondaryRateLimit
        );
        assert!(paused(&app).is_none(), "{} limits stay below the threshold", hit + 1);
    }
    observe(&app.cfg.config_dir, &identity, limit, at(5));
    assert_eq!(paused(&app).unwrap().cause, Cause::SecondaryRateLimit);
    let status = status_json(&app, at(5)).await;
    assert_eq!(status["cause"], "secondary_rate_limit");
    assert_eq!(status["next_probe_at"], json!(at(10)));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_reconnected_token_closes_the_breaker_without_a_probe() {
    let root = std::env::temp_dir().join(format!("colonizer-breaker-reconnect-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    crate::util::write_secret(&app.github_token_file(), "old-token").unwrap();
    observe(&app.cfg.config_dir, &identity(&app), "gh: Bad credentials (HTTP 401)", at(0));
    assert_eq!(paused(&app).unwrap().cause, Cause::TokenRevoked);
    crate::util::write_secret(&app.github_token_file(), "new-token").unwrap();
    assert!(paused(&app).is_none(), "the new identity is not paused");
    let mut probed = false;
    assert!(
        probe_tick_with(&app, at(1), || {
            probed = true;
            async { Ok::<(), String>(()) }
        })
        .await
    );
    assert!(!probed, "a new token needs no probe");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn held_publishes_go_out_one_at_a_time_once_the_breaker_closes() {
    let root = std::env::temp_dir().join(format!("colonizer-breaker-held-{}", crate::util::short_id()));
    let app = crate::tests::test_app(&root);
    assert!(hold_publish(&app, "a").is_none(), "nothing is held while GitHub works");
    observe(
        &app.cfg.config_dir,
        &identity(&app),
        "gh: Sorry. Your account was suspended. (HTTP 403)",
        at(0),
    );
    for id in ["a", "b", "a", "c"] {
        assert!(hold_publish(&app, id).is_some());
    }
    assert_eq!(status_json(&app, at(1)).await["held_publishes"], 3, "a colony is held once");

    // Still open: nothing is released.
    let released = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
    let record = |released: std::sync::Arc<Mutex<Vec<String>>>| {
        let in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        move |_app: Shared, id: String| {
            let released = released.clone();
            let in_flight = in_flight.clone();
            async move {
                assert_eq!(
                    in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                    0,
                    "one at a time"
                );
                tokio::task::yield_now().await;
                released.lock().unwrap().push(id);
                in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    };
    release_held_with(&app, record(released.clone())).await;
    assert!(released.lock().unwrap().is_empty());

    assert!(probe_tick_with(&app, at(30), || async { Ok::<(), String>(()) }).await);
    release_held_with(&app, record(released.clone())).await;
    assert_eq!(*released.lock().unwrap(), ["a", "b", "c"], "oldest first");
    assert_eq!(
        status_json(&app, at(31)).await,
        json!({"paused": false, "secondary_limits_recent": 0})
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn the_status_route_names_the_cause_and_the_next_step() {
    let (root, app) = queued_app("status").await;
    let get = |app: Shared| async move {
        let res = routes()
            .with_state(app)
            .oneshot(Request::builder().uri("/api/github/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    };
    assert_eq!(get(app.clone()).await["paused"], false);

    observe(
        &app.cfg.config_dir,
        &identity(&app),
        "gh: Bad credentials (HTTP 401)",
        Utc::now(),
    );
    let status = get(app.clone()).await;
    assert_eq!(status["paused"], true);
    assert_eq!(status["cause"], "token_revoked");
    assert_eq!(status["message"], "Token revoked");
    assert_eq!(status["next_step"], "reconnect GitHub in Settings → Connections");
    assert_eq!(status["queued"], 1);
    assert_eq!(status["detail"], "gh: Bad credentials (HTTP 401)");

    let _ = app.gh(["api", "user"]);
    assert_eq!(get(app.clone()).await["refused_calls"], 1, "a refused call is counted");
    assert_eq!(Cause::Suspended.headline(), "GitHub account suspended");
    assert_eq!(Cause::Suspended.next_step(), "contact GitHub support");
    let _ = std::fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------------------------
// No call site may bypass the breaker.
// ---------------------------------------------------------------------------------------------

/// Every command that reaches GitHub is built by `App::gh` or `App::git_remote`, which check the
/// breaker. A new `Command::new("gh")`, a new caller of the unguarded builder or of the network
/// git outside `github.rs` would bypass it, so this fails until the new site goes through them.
#[test]
fn no_call_site_builds_a_github_command_around_the_breaker() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    let this = Path::new(file!()).file_name().unwrap();
    let mut found: HashMap<&str, Vec<String>> = HashMap::new();
    for file in &files {
        let rel = file.strip_prefix(&src).unwrap().display().to_string();
        if rel == format!("github_breaker/{}", this.to_string_lossy()) {
            continue;
        }
        let text = std::fs::read_to_string(file).unwrap();
        for needle in ["Command::new(\"gh\")", "gh_unguarded(", "gh_program()", "git_network("] {
            for _ in text.matches(needle) {
                found.entry(needle).or_default().push(rel.clone());
            }
        }
    }
    // `set_token` checks a token the operator is pasting in — a new identity, not the paused one.
    assert_eq!(found.get("Command::new(\"gh\")").cloned().unwrap_or_default(), ["github.rs"]);
    for needle in ["gh_unguarded(", "gh_program()"] {
        for file in found.get(needle).cloned().unwrap_or_default() {
            assert!(
                file == "github.rs" || file == "github_breaker.rs",
                "{file} calls {needle} around the GitHub breaker; use App::gh"
            );
        }
    }
    for file in found.get("git_network(").cloned().unwrap_or_default() {
        assert_eq!(
            file, "github.rs",
            "{file} builds network git around the breaker; use App::git_remote"
        );
    }
}
