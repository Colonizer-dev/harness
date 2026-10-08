//! The feed's privacy boundary and its two routes.
//!
//! The tests are grouped by what they defend: what a field may be, what an activity line may
//! become, and what the two routes answer to a caller who is not who they say they are.

use super::*;
use crate::tests::test_app;
use axum::body::Body;
use axum::http::{Method, Request, header};
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use std::path::PathBuf;
use tower::ServiceExt as _;

const HOST: &str = "colonizer-test";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A throwaway install whose `[public_feed]` block is `toml`. The config dir has to exist: the
/// key store and `runtime::host_id` both write into it.
fn install(tag: &str, toml: &str) -> (Shared, PathBuf) {
    let root = std::env::temp_dir().join(format!("colonizer-feed-{tag}-{}", crate::util::short_id()));
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("colonizer.toml"), toml).unwrap();
    (test_app(&root), config_dir)
}

/// The feed switched on, publishing one repository.
fn publishing(tag: &str) -> (Shared, PathBuf) {
    install(
        tag,
        "[public_feed]\nenabled = true\nrepos = [\"acme/web\"]\nhistory_limit = 50\n",
    )
}

/// One activity line: a colony working an issue in `acme/web`, at a known time and sequence.
fn entry(seq: u64, kind: &str) -> crate::activity::Entry {
    crate::activity::Entry {
        seq,
        ts: format!("2026-10-{:02}T12:00:00Z", (seq % 27) + 1),
        kind: kind.to_string(),
        actor: "colony".to_string(),
        colony: Some("ab12cd34".to_string()),
        repo: Some("acme/web".to_string()),
        issue: Some(895),
        ..crate::activity::Entry::default()
    }
}

/// The same, carrying the lines an activity log holds and the feed must not: a task, a reason, a
/// path, a token.
fn loaded_entry(seq: u64, kind: &str) -> crate::activity::Entry {
    crate::activity::Entry {
        title: Some("Prompt: rewrite the auth middleware".to_string()),
        detail: Some("failed at src/auth.rs:42: ANTHROPIC_API_KEY=sk-live-abc".to_string()),
        target: Some("provider openrouter".to_string()),
        ..entry(seq, kind)
    }
}

/// Writes lines to the activity log the way `activity::record` lays it out, so the feed reads them
/// through the same reader `/api/activity` uses.
fn write_log(app: &App, entries: &[crate::activity::Entry]) {
    let text = entries
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(app.cfg.data_dir.join("activity.jsonl"), format!("{text}\n")).unwrap();
}

/// The feed switched on for `acme/web`.
fn cfg() -> FeedConfig {
    FeedConfig {
        enabled: true,
        repos: vec!["acme/web".to_string()],
        hosts: Vec::new(),
        history_limit: None,
    }
}

fn cfg_publishing_nothing() -> FeedConfig {
    FeedConfig {
        repos: Vec::new(),
        ..cfg()
    }
}

/// The feed's two routes with the app as state — the same routes `features::ALL` mounts.
fn feed_router(app: &Shared) -> axum::Router {
    routes().with_state(app.clone())
}

/// A GET to the feed, with the pieces a caller controls: a key, a `Last-Event-ID`, and the peer
/// address a socket would have reported. `None` is a server that was never given one.
fn get(uri: &str, key: Option<&str>, last_event_id: Option<&str>, peer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(key) = key {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    if let Some(last) = last_event_id {
        builder = builder.header("last-event-id", last);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    if let Some(addr) = peer
        .map(|peer| format!("{peer}:44321").parse::<SocketAddr>())
        .transpose()
        .unwrap()
    {
        // Exactly what `into_make_service_with_connect_info` puts on a request it served.
        request.extensions_mut().insert(ConnectInfo(addr));
    }
    request
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn feed_key(config_dir: &Path, ips: &[&str], rate: u32) -> String {
    let (plaintext, _) = keys::create(
        config_dir,
        keys::NewKey {
            name: "test site".to_string(),
            ip_allowlist: ips.iter().map(|s| s.to_string()).collect(),
            rate_limit_per_minute: Some(rate),
        },
    )
    .unwrap();
    plaintext
}

/// The raw SSE blocks a stream emits, up to `want` of them. Reading stops when the body ends or
/// the stream goes quiet, so a test asserts on what arrived rather than waiting forever.
async fn read_blocks(body: Body, want: usize) -> Vec<String> {
    let mut body = body;
    read_frames(&mut body, want, Duration::from_secs(10)).await
}

/// The same, off an open body so a test can read again after changing the log underneath the
/// stream, and with its own quiet window so asserting that *nothing* arrived is not a ten-second
/// wait per attempt.
async fn read_frames(body: &mut Body, want: usize, quiet: Duration) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    while out.len() < want {
        let Ok(Some(Ok(frame))) = tokio::time::timeout(quiet, body.frame()).await else {
            break;
        };
        let Ok(data) = frame.into_data() else { continue };
        buf.push_str(&String::from_utf8_lossy(&data));
        while let Some(end) = buf.find("\n\n") {
            out.push(buf[..end].to_string());
            buf = buf[end + 2..].to_string();
        }
    }
    out
}

/// One SSE block's `id:` and `data:` fields.
fn id_of(block: &str) -> String {
    field_of(block, "id")
}

fn data_of(block: &str) -> Value {
    serde_json::from_str(&field_of(block, "data")).unwrap()
}

fn field_of(block: &str, name: &str) -> String {
    block
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(": ").map(str::to_string))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The field set: what a third party can read at all
// ---------------------------------------------------------------------------

/// The heart of #895. If a field is added to [`FeedEvent`] this fails, which is the point: the
/// published set is a decision, not a byproduct of what the projector happens to copy.
#[test]
fn the_serialized_field_set_is_exactly_the_ten_published_fields() {
    let event = FeedEvent {
        id: format!("{HOST}:7"),
        kind: FeedKind::PrOpened,
        colony: pseudonym("ab12cd34"),
        repo: Some("acme/web".to_string()),
        issue: Some(895),
        pr_url: Some("https://github.com/acme/web/pull/1".to_string()),
        status: Some("pr_opened".to_string()),
        ts: Utc::now(),
        host: HOST.to_string(),
        intensity: Some(3),
    };
    let value = serde_json::to_value(&event).unwrap();
    let mut fields: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "colony",
            "host",
            "id",
            "intensity",
            "issue",
            "kind",
            "pr_url",
            "repo",
            "status",
            "ts"
        ]
    );

    // Every field but `id`, `kind`, `colony`, `repo`, `issue`, `ts` and `host` is optional and stays
    // out of the body when unset, so a reader cannot tell "absent" from "empty" for a colony that
    // never opened a pull request.
    let bare = FeedEvent {
        pr_url: None,
        status: None,
        intensity: None,
        ..event
    };
    let object = serde_json::to_value(&bare).unwrap();
    let object = object.as_object().unwrap();
    assert!(!object.contains_key("pr_url"));
    assert!(!object.contains_key("status"));
    assert!(!object.contains_key("intensity"));
}

#[test]
fn a_colony_is_published_as_a_pseudonym_never_as_its_id() {
    let event = project(&entry(1, "colony.launch"), &cfg(), HOST).unwrap();
    assert_ne!(event.colony, "ab12cd34");
    assert_eq!(event.colony.len(), PSEUDONYM_BYTES * 2);
    assert_eq!(event.colony, pseudonym("ab12cd34"));
    // Stable, so a site can follow one colony across events.
    assert_eq!(event.colony, project(&entry(2, "colony.tick"), &cfg(), HOST).unwrap().colony);
    // And the id is what a `Last-Event-ID` resumes from, never the colony.
    assert_eq!(event.id, format!("{HOST}:1"));
}

/// The reason the field set is closed: what the activity log holds about a task is never read.
#[test]
fn a_colonys_task_reason_and_path_never_reach_the_feed() {
    let event = project(&loaded_entry(1, "outcome.failed"), &cfg(), HOST).unwrap();
    let json = serde_json::to_string(&event).unwrap();
    for leak in ["Prompt:", "middleware", "src/auth.rs", "sk-live-abc", "openrouter"] {
        assert!(!json.contains(leak), "{leak} leaked into {json}");
    }
    // The line still says what happened, at the coarsest level a site can draw.
    assert_eq!(event.kind, FeedKind::Failed);
    assert_eq!(event.status.as_deref(), Some("failed"));
}

/// There is no title in the feed at all, under any name. `Session::issue_title` looks like GitHub
/// text but is operator-typed like everything else — `POST /api/sessions` copies the caller's
/// `title` in, `handoff` falls back to a chat transcript's title — so publishing it would put a
/// task on the internet behind a reassuring field name. The issue *number* survives; the words do
/// not.
#[test]
fn no_title_of_any_kind_is_published_and_the_issue_number_still_is() {
    let event = project(&loaded_entry(1, "colony.launch"), &cfg(), HOST).unwrap();
    let json = serde_json::to_string(&event).unwrap();
    for leak in ["Prompt:", "middleware", "issue_title", "title"] {
        assert!(!json.contains(leak), "{leak} leaked into {json}");
    }
    assert_eq!(event.issue, Some(895), "the number links to public GitHub, so it stays");

    // The activity log's own `title` is where a task rides out under a kind name, and it is
    // `entry.summary` whenever a colony has no issue title at all.
    let mut summary_titled = entry(2, "colony.launch");
    summary_titled.title = Some("Rewrite the auth middleware".to_string());
    let json = serde_json::to_string(&project(&summary_titled, &cfg(), HOST).unwrap()).unwrap();
    assert!(!json.contains("middleware"), "{json}");
}

// ---------------------------------------------------------------------------
// What becomes an event, and what does not
// ---------------------------------------------------------------------------

#[test]
fn the_activity_kinds_map_to_the_feeds_own_vocabulary() {
    let cases = [
        ("colony.launch", FeedKind::Started),
        ("colony.tick", FeedKind::Tick),
        ("outcome.question", FeedKind::Asking),
        ("outcome.pr_opened", FeedKind::PrOpened),
        ("outcome.merged", FeedKind::Merged),
        ("outcome.failed", FeedKind::Failed),
        ("outcome.stopped", FeedKind::Stopped),
    ];
    for (activity, feed) in cases {
        assert_eq!(kind_of(activity), Some(feed), "{activity}");
    }
    // A kind the feed does not know is a heartbeat: a new activity line shows up as a colony
    // working rather than vanishing, but it cannot invent a new kind.
    assert_eq!(kind_of("colony.something_new"), Some(FeedKind::Tick));
    // Conversation and installation lines are about the install, not a colony, and are never
    // published: `chat.*` is the one place prompt text lives.
    for kind in [
        "chat.user",
        "chat.agent",
        "remote.push",
        "redteam.run",
        "secret.save",
        "settings.remove",
    ] {
        assert_eq!(kind_of(kind), None, "{kind} must not be publishable");
    }
    // Every kind spells itself the way the JSON carries it.
    assert_eq!(FeedKind::PrOpened.as_str(), "pr_opened");
    assert_eq!(serde_json::to_value(FeedKind::PrOpened).unwrap(), json!("pr_opened"));
}

#[test]
fn a_repository_that_has_not_opted_in_is_dropped_rather_than_blanked() {
    let allowlist = cfg();
    let mut private = entry(1, "colony.launch");
    private.repo = Some("acme/private".to_string());
    assert!(
        project(&private, &allowlist, HOST).is_none(),
        "an unopted repository must not be published"
    );
    // Case-insensitively: GitHub names are case-insensitive and so is the allowlist.
    let mut shouted = entry(2, "colony.launch");
    shouted.repo = Some("ACME/Web".to_string());
    assert!(project(&shouted, &allowlist, HOST).is_some());
    // A line with no colony or no repository has nothing to draw and is dropped too.
    let mut no_repo = entry(3, "colony.launch");
    no_repo.repo = None;
    assert!(project(&no_repo, &allowlist, HOST).is_none());
    let mut no_colony = entry(4, "colony.launch");
    no_colony.colony = None;
    assert!(project(&no_colony, &allowlist, HOST).is_none());
    // An empty allowlist publishes nothing at all, so switching the feed on cannot leak by
    // omission.
    assert!(project(&entry(5, "colony.launch"), &cfg_publishing_nothing(), HOST).is_none());
    // A `hosts` list that does not name this install is the same silence, and the default table
    // is off, so the feed cannot be live unless somebody said so.
    assert!(cfg().publishes_host(HOST), "an empty hosts list means this install alone");
    let elsewhere = FeedConfig {
        hosts: vec!["another-host".to_string()],
        ..cfg()
    };
    assert!(!elsewhere.publishes_host(HOST));
    assert!(!FeedConfig::default().enabled);
}

#[test]
fn a_line_with_an_unreadable_time_is_not_published_with_a_made_up_one() {
    let mut broken = entry(1, "colony.launch");
    broken.ts = "last tuesday".to_string();
    assert!(project(&broken, &cfg(), HOST).is_none());
}

/// `pr_url` is free text off the activity log, so it is validated rather than copied: a site turns
/// it into a link, and a link a browser resolves as `javascript:` or a `data:` blob is a hole in
/// exactly the place the feed promises it has none. A newline would also split one SSE frame.
#[test]
fn a_pr_url_is_published_only_when_it_is_a_plain_http_url_and_it_is_clipped() {
    let with_pr = |url: &str| {
        let mut e = entry(1, "outcome.pr_opened");
        e.pr_url = Some(url.to_string());
        project(&e, &cfg(), HOST).unwrap().pr_url
    };

    assert_eq!(
        with_pr("https://github.com/acme/web/pull/12"),
        Some("https://github.com/acme/web/pull/12".to_string())
    );
    assert_eq!(
        with_pr("http://github.com/acme/web/pull/12"),
        Some("http://github.com/acme/web/pull/12".to_string()),
        "plain http is a URL a site can link to; it is not a reason to refuse the event"
    );

    // Not http(s): a `javascript:` or `data:` URL here would be executed by whoever follows it.
    for bad in [
        "javascript:alert(document.cookie)",
        "data:text/html,<script>alert(1)</script>",
        "file:///etc/passwd",
        "ftp://github.com/acme/web/pull/12",
    ] {
        assert_eq!(with_pr(bad), None, "{bad} must not be published");
    }
    // Malformed, or carrying whitespace or a control character: a newline in one event would
    // split it across two SSE frames.
    for bad in ["not a url", "https://", "https://exa mple.com/x", "https://x.test/\nX: 1"] {
        assert_eq!(with_pr(bad), None, "{bad:?} must not be published");
    }
    // And it is clipped, so a megabyte of URL cannot ride out as one event.
    let long = format!("https://github.com/acme/web/pull/{}", "1".repeat(MAX_PR_URL * 2));
    let clipped = with_pr(&long).unwrap();
    assert!(clipped.chars().count() <= MAX_PR_URL + 1, "{} chars", clipped.chars().count());

    // A line with no pull request simply has none.
    assert!(
        project(&entry(2, "outcome.pr_opened"), &cfg(), HOST)
            .unwrap()
            .pr_url
            .is_none()
    );
}

#[test]
fn a_colony_ticks_at_most_once_a_minute_and_its_real_news_is_never_thinned() {
    let entries: Vec<_> = (1..=4)
        .map(|seq| crate::activity::Entry {
            ts: format!("2026-10-08T12:0{seq}:00Z"),
            ..entry(seq, "colony.tick")
        })
        .chain([entry(5, "colony.launch"), entry(6, "outcome.pr_opened")])
        .collect();
    let kinds: Vec<_> = events_from(&entries, &cfg(), HOST, &mut HashMap::new())
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    // Ticks one minute apart each survive; a burst inside one minute would keep only one. The
    // launch and the pull request are news and are never thinned.
    assert_eq!(kinds, ["tick", "tick", "tick", "tick", "started", "pr_opened"]);

    let burst: Vec<_> = (1..=4)
        .map(|seq| crate::activity::Entry {
            ts: format!("2026-10-08T12:00:0{seq}Z"),
            ..entry(seq, "colony.tick")
        })
        .collect();
    assert_eq!(
        events_from(&burst, &cfg(), HOST, &mut HashMap::new()).len(),
        1,
        "a burst of ticks is one heartbeat"
    );

    // The limit is per colony: a second colony ticking in the same window keeps its own.
    let other = crate::activity::Entry {
        colony: Some("beef0000".into()),
        ..entry(2, "colony.tick")
    };
    let both = [burst[0].clone(), other];
    assert_eq!(events_from(&both, &cfg(), HOST, &mut HashMap::new()).len(), 2);
}

/// The stream's tick limit has to span polls, not restart at each one. `pump` reads only the lines
/// that arrived since the last poll, so a fresh map per read would hold at most one tick per
/// colony and the 60 s check could never fire: a colony ticking every second would publish thirty
/// events a minute instead of one. The map is carried in, which is what this asserts.
#[test]
fn a_tick_limit_carried_across_reads_thins_what_each_read_alone_cannot() {
    let tick = |seq: u64, at: &str| crate::activity::Entry {
        ts: at.to_string(),
        ..entry(seq, "colony.tick")
    };
    let mut last_tick = HashMap::new();

    // One window: the first tick publishes.
    assert_eq!(
        events_from(&[tick(1, "2026-10-08T12:00:00Z")], &cfg(), HOST, &mut last_tick).len(),
        1
    );
    // The next four polls each arrive as their own one-line window, a few seconds apart. Thinned
    // individually every one of them would publish, because no single window holds two ticks.
    for seq in 2..=5 {
        let window = [tick(seq, &format!("2026-10-08T12:00:0{seq}Z"))];
        assert!(
            events_from(&window, &cfg(), HOST, &mut last_tick).is_empty(),
            "seq {seq} is inside the minute and must be thinned"
        );
    }
    // Past the minute, the colony is heard again.
    assert_eq!(
        events_from(&[tick(6, "2026-10-08T12:01:01Z")], &cfg(), HOST, &mut last_tick).len(),
        1,
        "a minute later the next heartbeat is published"
    );
    // And a fresh map, which is what the old per-read code built, publishes all five.
    let mut rebuilt = HashMap::new();
    assert_eq!(
        events_from(&[tick(2, "2026-10-08T12:00:02Z")], &cfg(), HOST, &mut rebuilt).len(),
        1
    );
}

#[test]
fn a_colony_that_has_finished_is_not_in_the_active_list() {
    let entries = vec![
        entry(1, "colony.launch"),
        crate::activity::Entry {
            colony: Some("beef0000".into()),
            ..entry(2, "colony.launch")
        },
        entry(3, "outcome.merged"),
    ];
    let events = events_from(&entries, &cfg(), HOST, &mut HashMap::new());
    assert_eq!(events.len(), 3);
    let active = active_colonies(&events);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["colony"], pseudonym("beef0000"));
    assert_eq!(active[0]["status"], json!("running"));
}

// ---------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_feed_that_is_switched_off_is_a_404_on_both_routes() {
    let (app, config_dir) = install("off", "");
    let key = feed_key(&config_dir, &[], 60);
    for uri in ["/api/public/feed", "/api/public/feed/stream"] {
        // With no key at all an off feed still answers 404 and not 401: a feature that does not
        // exist should not be discoverable by its error codes.
        let res = feed_router(&app).oneshot(get(uri, None, None, None)).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri} with no key");
        let res = feed_router(&app).oneshot(get(uri, Some(&key), None, None)).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri} with a key");
    }
}

#[tokio::test]
async fn a_feed_switched_on_with_no_allowlisted_repository_is_an_empty_feed_not_a_leak() {
    let (app, config_dir) = install("empty", "[public_feed]\nenabled = true\n");
    let key = feed_key(&config_dir, &[], 60);
    write_log(&app, &[entry(1, "colony.launch")]);
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed", Some(&key), None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["events"], json!([]));
}

#[tokio::test]
async fn an_unknown_empty_or_revoked_key_is_one_401() {
    let (app, config_dir) = publishing("auth");
    let (revoked, meta) = keys::create(
        &config_dir,
        keys::NewKey {
            name: "revoked".into(),
            ip_allowlist: Vec::new(),
            rate_limit_per_minute: None,
        },
    )
    .unwrap();
    keys::revoke(&config_dir, &meta.id);
    for key in [
        None,
        Some(""),
        Some("cfd_never_issued"),
        Some(&revoked),
        Some("not-even-a-key"),
    ] {
        let res = feed_router(&app)
            .oneshot(get("/api/public/feed", key, None, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{key:?}");
    }
    // The other route is behind the same check.
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed/stream", None, None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_key_presented_from_outside_its_allowlist_is_403_and_a_forged_header_is_ignored() {
    let (app, config_dir) = publishing("ips");
    let key = feed_key(&config_dir, &["203.0.113.0/24"], 60);
    let outside = "198.51.100.7";
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed", Some(&key), None, Some(outside)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // `X-Forwarded-For` is the client's own claim about itself. Honouring it would let any caller
    // name an address inside any allowlist by adding a header.
    let mut forged = get("/api/public/feed", Some(&key), None, Some(outside));
    forged
        .headers_mut()
        .insert("x-forwarded-for", header::HeaderValue::from_static("203.0.113.9"));
    let res = feed_router(&app).oneshot(forged).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "a forwarded header must not grant an address"
    );

    // The same key from a listed address works, and from the next block over does not.
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed", Some(&key), None, Some("203.0.113.9")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed", Some(&key), None, Some("203.0.114.9")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // A server that was never given the peer's address cannot judge the allowlist, so the key is
    // then the whole control. docs/protocol/public-feed.md says so out loud.
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed", Some(&key), None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

/// The one way to reach the feed with *no* address at all: `remote::serve_connection` builds the
/// request by hand and puts only the `Tunnelled` marker on it. The allowlist check would be
/// skipped rather than enforced — `plausible_path` accepts any `/`-prefixed path — so the feed is
/// not served over the tunnel at all, on either route, with or without a valid key.
#[tokio::test]
async fn the_feed_is_refused_through_the_tunnel_rather_than_skipping_the_allowlist() {
    let (app, config_dir) = publishing("tunnel");
    let key = feed_key(&config_dir, &["203.0.113.0/24"], 60);
    for uri in ["/api/public/feed", "/api/public/feed/stream"] {
        for presented in [Some(key.as_str()), None] {
            let mut request = get(uri, presented, None, None);
            request.extensions_mut().insert(crate::remote::Tunnelled {
                host: "the-link.example".to_string(),
            });
            let res = feed_router(&app).oneshot(request).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri} with key {presented:?}");
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
            let body = String::from_utf8_lossy(&bytes);
            assert!(
                body.contains("not available over the remote tunnel"),
                "the 403 has to say why: {body}"
            );
        }
    }
    // The same request without the marker is the ordinary case and still works, so this is the
    // tunnel being refused and not the route being broken.
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed", Some(&key), None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_key_over_its_rate_gets_429() {
    let (app, config_dir) = publishing("rate");
    let key = feed_key(&config_dir, &[], 2);
    for i in 0..2 {
        let res = feed_router(&app)
            .oneshot(get("/api/public/feed", Some(&key), None, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "request {i}");
    }
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed", Some(&key), None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn the_snapshot_answers_events_and_the_colonies_still_in_flight() {
    let (app, config_dir) = publishing("snapshot");
    let key = feed_key(&config_dir, &[], 60);
    let mut private = entry(2, "colony.launch");
    private.repo = Some("acme/private".into());
    write_log(&app, &[entry(1, "colony.launch"), private, loaded_entry(3, "outcome.merged")]);

    let res = feed_router(&app)
        .oneshot(get("/api/public/feed", Some(&key), None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json",
        "a snapshot is JSON, not an event stream"
    );
    let body = body_json(res).await;
    let events = body["events"].as_array().unwrap();
    // Two of the three lines publish: the private repository's does not, and it does not leave a
    // hole — the sequence after it still appears.
    assert_eq!(events.len(), 2, "{body}");
    let host = crate::runtime::host_id(&app);
    assert_eq!(events[0]["id"], format!("{host}:1"));
    assert_eq!(events[0]["kind"], json!("started"));
    assert_eq!(events[1]["id"], format!("{host}:3"));
    assert_eq!(events[1]["kind"], json!("merged"));
    assert_eq!(events[1]["colony"], pseudonym("ab12cd34"));
    // The colony merged, so it is not among the ones still in flight.
    assert_eq!(body["active"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn the_snapshot_honours_history_limit() {
    let (app, config_dir) = install(
        "limit",
        "[public_feed]\nenabled = true\nrepos = [\"acme/web\"]\nhistory_limit = 2\n",
    );
    let key = feed_key(&config_dir, &[], 60);
    let entries: Vec<_> = (1..=6).map(|seq| entry(seq, "colony.launch")).collect();
    write_log(&app, &entries);
    let body = body_json(
        feed_router(&app)
            .oneshot(get("/api/public/feed", Some(&key), None, None))
            .await
            .unwrap(),
    )
    .await;
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    // The newest survive, because a site drawing colonies wants what is happening now.
    assert_eq!(events[1]["id"], format!("{}:6", crate::runtime::host_id(&app)));
}

// ---------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_stream_replays_the_recent_past_and_then_resumes_exactly_where_it_was_told_to() {
    let (app, config_dir) = publishing("stream");
    let key = feed_key(&config_dir, &[], 60);
    let mut private = entry(4, "colony.launch");
    private.repo = Some("acme/private".into());
    write_log(
        &app,
        &[
            entry(1, "colony.launch"),
            entry(2, "colony.tick"),
            private,
            entry(5, "outcome.merged"),
        ],
    );
    let host = crate::runtime::host_id(&app);

    // With no header a stream starts at the oldest retained line, so a client that connects gets
    // the recent past rather than an empty screen until the next colony moves. Sequence 4 is the
    // private repository's line and never appears, but it does not hold the cursor back.
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed/stream", Some(&key), None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get(header::CONTENT_TYPE).unwrap(), "text/event-stream");
    let blocks = read_blocks(res.into_body(), 3).await;
    let ids: Vec<String> = blocks.iter().map(|b| id_of(b)).collect();
    assert_eq!(ids, [format!("{host}:1"), format!("{host}:2"), format!("{host}:5")]);
    assert!(blocks[0].contains("event: started"), "first block was: {}", blocks[0]);
    assert_eq!(data_of(&blocks[1])["colony"], json!(pseudonym("ab12cd34")));
    assert_eq!(data_of(&blocks[1])["issue"], json!(895));

    // A reconnect with `Last-Event-ID` resumes immediately after the line it last saw: no gap, and
    // no replay of what the client already has.
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed/stream", Some(&key), Some(&format!("{host}:2")), None))
        .await
        .unwrap();
    let blocks = read_blocks(res.into_body(), 1).await;
    assert_eq!(blocks.len(), 1);
    assert_eq!(id_of(&blocks[0]), format!("{host}:5"));
    assert_eq!(data_of(&blocks[0])["kind"], json!("merged"));

    // A `Last-Event-ID` that does not parse is a full replay, not silence.
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed/stream", Some(&key), Some("garbage"), None))
        .await
        .unwrap();
    let blocks = read_blocks(res.into_body(), 1).await;
    assert_eq!(id_of(&blocks[0]), format!("{host}:1"));
}

/// A read that comes back empty is a rotation in progress or a transient read failure, not an
/// empty log. Treating it as one rewinds the cursor to 0, and the next poll replays the whole
/// retained history at a client that already has all of it — a site drawing colonies sees every
/// colony restart from the beginning. The cursor must survive an empty read untouched.
#[tokio::test]
async fn an_empty_read_leaves_the_cursor_alone_instead_of_replaying_the_whole_log() {
    let (app, config_dir) = publishing("cursor");
    let key = feed_key(&config_dir, &[], 60);
    let history: Vec<_> = (1..=6).map(|seq| entry(seq, "colony.launch")).collect();
    write_log(&app, &history);
    let host = crate::runtime::host_id(&app);

    let res = feed_router(&app)
        .oneshot(get("/api/public/feed/stream", Some(&key), None, None))
        .await
        .unwrap();
    let mut body = res.into_body();
    let ids: Vec<String> = read_frames(&mut body, 6, Duration::from_secs(10))
        .await
        .iter()
        .map(|b| id_of(b))
        .collect();
    assert_eq!(ids, (1..=6).map(|seq| format!("{host}:{seq}")).collect::<Vec<_>>());

    // The log goes empty under the stream — a rotation caught mid-write, or a read error.
    std::fs::write(app.cfg.data_dir.join("activity.jsonl"), "").unwrap();
    // Long enough for the pump to poll a few times and come back empty.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        read_frames(&mut body, 1, Duration::from_millis(300)).await.is_empty(),
        "an empty log must not publish anything"
    );

    // And then it comes back with the same six lines and a new one. The client is past all six, so
    // only the seventh is news; re-emitting the first six is the replay this asserts against.
    let mut restored = history.clone();
    restored.push(entry(7, "outcome.merged"));
    write_log(&app, &restored);
    let blocks = read_frames(&mut body, 1, Duration::from_secs(10)).await;
    assert_eq!(
        id_of(&blocks[0]),
        format!("{host}:7"),
        "an empty read must not rewind the cursor and replay the retained log"
    );
    assert!(
        read_frames(&mut body, 1, Duration::from_millis(300)).await.is_empty(),
        "and nothing else follows it"
    );
}

#[tokio::test]
async fn a_resume_point_the_log_has_rotated_past_is_named_rather_than_silently_skipped() {
    let (app, config_dir) = publishing("gap");
    let key = feed_key(&config_dir, &[], 60);
    write_log(&app, &[entry(9, "colony.launch")]);
    let host = crate::runtime::host_id(&app);
    let res = feed_router(&app)
        .oneshot(get("/api/public/feed/stream", Some(&key), Some(&format!("{host}:1")), None))
        .await
        .unwrap();
    let blocks = read_blocks(res.into_body(), 2).await;
    // The gap is named before the retained line is replayed, so a site drawing a colony trail
    // learns that part of it is missing instead of drawing a broken one.
    assert!(blocks[0].starts_with("event: gap"), "first block was: {}", blocks[0]);
    let note = data_of(&blocks[0]);
    assert_eq!(note["requested_after"], json!(1));
    assert_eq!(note["oldest_retained"], json!(9));
    assert_eq!(id_of(&blocks[1]), format!("{host}:9"));
}
