//! The tests for `GET /metrics`: the exposition's grammar, the auth matrix, what a scrape must
//! never contain, the histogram's arithmetic, and the series cap.

use super::*;
use crate::config::ModuleChoice;
use crate::sessions::SessionStatus;
use crate::{Shared, sessions};
use axum::{
    body::Body,
    http::{Method, StatusCode, header},
    response::Response,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tower::ServiceExt as _;

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// A temp root that removes itself.
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn root(tag: &str) -> Root {
    let dir = std::env::temp_dir().join(format!("colonizer-metrics-{tag}-{}", crate::util::short_id()));
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    Root(dir)
}

/// An app whose observability module is enabled with `prometheus` on, and whatever settings the
/// test names on top.
async fn app_with_metrics(dir: &Path, settings: &[(&str, Value)]) -> Shared {
    let app = crate::tests::test_app(dir);
    let mut all = serde_json::Map::new();
    all.insert("prometheus".into(), json!(true));
    for (key, value) in settings {
        all.insert((*key).to_string(), value.clone());
    }
    app.modules.write().await.observability = Some(ModuleChoice {
        provider: crate::observability::settings::FILE.into(),
        enabled: true,
        settings: all,
    });
    app
}

/// An app with no observability module at all.
fn app_without_metrics(dir: &Path) -> Shared {
    crate::tests::test_app(dir)
}

/// The route behind the real `host_guard`, the way `api_tokens`' own auth tests drive theirs.
fn router(app: &Shared) -> axum::Router {
    axum::Router::new()
        .route(PATH, axum::routing::get(scrape))
        .layer(axum::middleware::from_fn_with_state(app.clone(), crate::server::host_guard))
        .with_state(app.clone())
}

fn get(uri: &str, bearer: Option<&str>) -> axum::extract::Request {
    let mut builder = axum::http::Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7878");
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::empty()).unwrap()
}

/// The same request, authenticated as whoever owns the cookie the browser would carry.
fn get_with_cookie(uri: &str, cookie: &str) -> axum::extract::Request {
    axum::http::Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:7878")
        .header(header::COOKIE, format!("{}={cookie}", crate::auth::COOKIE_NAME))
        .body(Body::empty())
        .unwrap()
}

async fn text(res: Response) -> String {
    String::from_utf8(axum::body::to_bytes(res.into_body(), 1 << 24).await.unwrap().to_vec()).unwrap()
}

/// A colony with a recognizable id, org and agent, so a test can look for them in a scrape.
fn colony(org: &str, repo: &str, status: SessionStatus) -> sessions::Session {
    let mut s = sessions::tests::colony(org, status);
    s.id = "canary-colony-id".into();
    s.repo = format!("{org}/{repo}");
    s.agent = "canary-agent".into();
    s
}

async fn seed_sessions(app: &Shared, sessions: Vec<sessions::Session>) {
    *app.sessions.write().await = sessions;
}

// ---------------------------------------------------------------------------
// The grammar.
// ---------------------------------------------------------------------------

/// Every line is one of the three legal forms of the 0.0.4 text format, the name is a legal metric
/// name, `le` labels carry a legal value, and every catalogue metric is declared before any of its
/// samples. This is the test that would catch a renderer that emitted a stray `#` in a label value
/// or a `NaN` where a number belongs.
#[test]
fn every_line_of_a_scrape_is_a_legal_exposition_line() {
    let body = render(&seeded_snapshot(), &plain());
    let name_ok = |name: &str| {
        let mut chars = name.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == ':')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
    };
    let mut declared: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("# HELP ") {
            let (name, _help) = rest.split_once(' ').expect("a HELP line names a metric and a text");
            assert!(name_ok(name), "illegal metric name in {line:?}");
            assert!(
                declared.entry(name.to_string()).or_default().insert("help"),
                "{name} has two HELP lines"
            );
        } else if let Some(rest) = line.strip_prefix("# TYPE ") {
            let (name, kind) = rest.split_once(' ').expect("a TYPE line names a metric and a type");
            assert!(name_ok(name), "illegal metric name in {line:?}");
            assert!(matches!(kind, "counter" | "gauge" | "histogram"), "illegal type in {line:?}");
            assert!(
                declared.entry(name.to_string()).or_default().insert("type"),
                "{name} has two TYPE lines"
            );
        } else {
            // `name{a="1",b="2"} 12`, `name 12`, and the histogram's `_bucket`/`_sum`/`_count`.
            let (series, value) = line.rsplit_once(' ').expect("a sample line ends in a value");
            let value: f64 = value
                .parse()
                .unwrap_or_else(|_| panic!("{value:?} is not a number in {line:?}"));
            assert!(value.is_finite(), "an exposition value must be finite: {line:?}");
            let (name, labels) = match series.split_once('{') {
                Some((name, rest)) => (name, Some(rest.strip_suffix('}').expect("labels close"))),
                None => (series, None),
            };
            assert!(name_ok(name), "illegal metric name in {line:?}");
            let Some(labels) = labels else { continue };
            for pair in split_labels(labels) {
                let (key, value) = pair.split_once("=\"").expect("a label is name=\"value\"");
                assert!(name_ok(key), "illegal label name in {line:?}");
                let value = value.strip_suffix('"').expect("a label value closes");
                assert!(
                    !value.contains(['\\', '"', '\n']),
                    "an unescaped character survived into a label value: {line:?}"
                );
                if key == "le" {
                    let ok = value == "+Inf" || value.parse::<f64>().is_ok_and(f64::is_finite);
                    assert!(ok, "illegal le value in {line:?}");
                }
            }
            // HELP and TYPE precede every sample of the metric they declare. A histogram declares
            // its base name and then samples `_bucket`, `_sum` and `_count`.
            let base = ["_bucket", "_sum", "_count"]
                .into_iter()
                .find_map(|suffix| name.strip_suffix(suffix).filter(|b| declared.contains_key(*b)))
                .unwrap_or(name);
            if !seen.insert(name.to_string()) {
                assert!(declared.contains_key(base), "{name} has samples but no HELP/TYPE");
            }
        }
    }
    for (name, what) in &declared {
        assert_eq!(what.len(), 2, "{name} needs both a HELP and a TYPE line");
    }
}

/// Splits `{a="1",b="2"}`'s body on the commas *between* pairs, never on a comma inside a value.
fn split_labels(labels: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;
    for (at, c) in labels.char_indices() {
        match c {
            '\\' if in_quotes => escaped = !escaped,
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes && !escaped => {
                out.push(&labels[start..at]);
                start = at + 1;
            }
            _ => escaped = false,
        }
    }
    if start < labels.len() {
        out.push(&labels[start..]);
    }
    out
}

/// A snapshot with one of everything the catalogue knows how to print.
fn seeded_snapshot() -> Snapshot {
    let latency = Histogram::default();
    for ms in [40, 300, 900, 4_000, 40_000] {
        latency.observe(ms);
    }
    Snapshot {
        colonies: vec![
            ("running".into(), "acme".into(), "claude-code".into(), 2),
            ("waiting_for_answer".into(), "other".into(), "claude-code".into(), 1),
        ],
        started: 11,
        finished: vec![1, 0, 0, 0, 0, 0, 2, 0, 0],
        providers: vec![crate::gateway::ProviderScrape {
            provider: "openrouter".into(),
            usage: crate::gateway::ProviderUsage {
                requests: 10,
                failures: 3,
                fallbacks: 2,
                ..Default::default()
            },
            in_flight: 2,
            queued: 1,
            latency: latency.snapshot(),
        }],
        tokens: vec![
            ("acme".into(), "claude-sonnet".into(), "total", 900),
            ("acme".into(), "all".into(), "input", 400),
            ("acme".into(), "all".into(), "output", 500),
        ],
        cost: vec![("acme".into(), "claude-sonnet".into(), 1.25)],
        questions_open: 1,
        attention: vec![("stalled".into(), 1)],
        queue_depth: 4,
        storage_alert: true,
        disk_free_bytes: Some(1024 * 1024 * 1024),
    }
}

fn plain() -> Hasher {
    Hasher::new(false, None)
}

/// The value a scrape reports for `name` with exactly these labels, or `None` when it has no such
/// series.
fn value_of(body: &str, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
    body.lines().find_map(|line| {
        let (series, value) = line.rsplit_once(' ')?;
        if series == name {
            return if labels.is_empty() { value.parse().ok() } else { None };
        }
        let (series_name, rest) = series.split_once('{')?;
        if series_name != name {
            return None;
        }
        let rest = rest.strip_suffix('}')?;
        let got: BTreeMap<&str, &str> = split_labels(rest)
            .into_iter()
            .map(|pair| {
                let (k, v) = pair.split_once("=\"").unwrap();
                (k, v.strip_suffix('"').unwrap())
            })
            .collect();
        if labels.iter().all(|(k, v)| got.get(k) == Some(v)) {
            value.parse().ok()
        } else {
            None
        }
    })
}

/// The whole catalogue, as a table: what a scrape must say about a snapshot it is given. One row
/// per series, so a family that starts reporting the wrong number names itself here.
#[test]
fn the_catalogue_reports_the_install_as_it_is() {
    let body = render(&seeded_snapshot(), &plain());
    /// One row: a metric, the labels that pick out a series, and the number it must carry.
    type Row = (&'static str, &'static [(&'static str, &'static str)], f64);
    let expected: &[Row] = &[
        (
            "colonizer_colonies",
            &[("status", "running"), ("org", "acme"), ("agent", "claude-code")],
            2.0,
        ),
        (
            "colonizer_colonies",
            &[("status", "waiting_for_answer"), ("org", "other")],
            1.0,
        ),
        ("colonizer_colonies_started_total", &[], 11.0),
        ("colonizer_colonies_finished_total", &[("outcome", "pr_opened")], 1.0),
        ("colonizer_colonies_finished_total", &[("outcome", "question")], 2.0),
        (
            "colonizer_gateway_requests_total",
            &[("provider", "openrouter"), ("outcome", "ok")],
            7.0,
        ),
        (
            "colonizer_gateway_requests_total",
            &[("provider", "openrouter"), ("outcome", "error")],
            3.0,
        ),
        ("colonizer_gateway_fallbacks_total", &[("provider", "openrouter")], 2.0),
        ("colonizer_gateway_in_flight", &[("provider", "openrouter")], 2.0),
        ("colonizer_gateway_queued", &[("provider", "openrouter")], 1.0),
        ("colonizer_questions_open", &[], 1.0),
        ("colonizer_attention", &[("reason", "stalled")], 1.0),
        ("colonizer_queue_depth", &[], 4.0),
        ("colonizer_storage_alert", &[], 1.0),
        ("colonizer_disk_free_bytes", &[], 1073741824.0),
        // Both token shapes: a model's own total, and the org's four per-kind totals.
        (
            "colonizer_tokens_total",
            &[("org", "acme"), ("model", "claude-sonnet"), ("type", "total")],
            900.0,
        ),
        (
            "colonizer_tokens_total",
            &[("org", "acme"), ("model", "all"), ("type", "input")],
            400.0,
        ),
        (
            "colonizer_tokens_total",
            &[("org", "acme"), ("model", "all"), ("type", "output")],
            500.0,
        ),
        (
            "colonizer_cost_usd_total",
            &[("org", "acme"), ("model", "claude-sonnet")],
            1.25,
        ),
    ];
    for (name, labels, value) in expected {
        assert_eq!(value_of(&body, name, labels), Some(*value), "{name} with {labels:?}");
    }
    // The two token shapes are two series, never one: the docs page says not to sum them, and the
    // only thing a test can enforce is that they are printed apart.
    assert!(body.contains("model=\"claude-sonnet\",type=\"total\""));
    assert!(body.contains("model=\"all\",type=\"output\""));
}

/// A family with nothing to say still prints its `HELP` and `TYPE`, and an unknown free-space
/// reading is no sample at all rather than a zero that reads as "the disk is full".
#[test]
fn an_empty_family_still_declares_itself_and_an_unmeasured_disk_has_no_sample() {
    let mut snapshot = seeded_snapshot();
    snapshot.disk_free_bytes = None;
    snapshot.attention.clear();
    snapshot.storage_alert = false;
    let body = render(&snapshot, &plain());
    assert!(body.contains("# TYPE colonizer_disk_free_bytes gauge"));
    assert!(value_of(&body, "colonizer_disk_free_bytes", &[]).is_none());
    assert!(body.contains("# TYPE colonizer_attention gauge"));
    assert!(value_of(&body, "colonizer_attention", &[("reason", "stalled")]).is_none());
    assert_eq!(value_of(&body, "colonizer_storage_alert", &[]), Some(0.0));
}

// ---------------------------------------------------------------------------
// Privacy.
// ---------------------------------------------------------------------------

/// A scrape names statuses, providers, models, agents and reasons — and nothing else. A colony id,
/// a repository name, an attention message or a failure's text must not reach it under any setting.
#[tokio::test]
async fn a_scrape_never_carries_a_colony_id_a_repository_or_a_sentence() {
    let dir = root("privacy");
    let app = app_with_metrics(&dir.0, &[]).await;
    let mut session = colony("canary-org", "canary-repo", SessionStatus::Failed);
    session.attention = Some(json!({
        "reason": "stalled",
        "since": "2026-01-01T00:00:00Z",
        "detail": "canary-free-text-message",
    }));
    session.instructions = "canary-issue-title".into();
    seed_sessions(&app, vec![session]).await;

    let body = text(router(&app).oneshot(get(PATH, Some(&app.api_token))).await.unwrap()).await;
    let again = text(router(&app).oneshot(get(PATH, Some(&app.api_token))).await.unwrap()).await;
    assert_eq!(body, again, "two scrapes of an unchanged install are byte-identical");
    for canary in [
        "canary-colony-id",
        "canary-repo",
        "canary-free-text-message",
        "canary-issue-title",
    ] {
        assert!(!body.contains(canary), "{canary} reached a scrape:\n{body}");
    }
    // The org is there in plain mode, and the repo never is: no `repo` label exists at all.
    assert!(body.contains("org=\"canary-org\""), "{body}");
    assert!(!body.contains("repo="), "there is no repo label in the catalogue:\n{body}");
}

#[tokio::test]
async fn hashed_repo_names_replace_every_org_and_leave_nothing_to_guess_from() {
    let dir = root("hashed");
    let app = app_with_metrics(&dir.0, &[("repo_names", json!("hashed"))]).await;
    // The key is loaded by `start_tasks`; a test drives the same path so the hashes are the ones a
    // real install would emit.
    start_tasks(&app);
    let mut settled = false;
    for _ in 0..200 {
        if hash_key().is_some() {
            settled = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(settled, "the hash key was never loaded");
    seed_sessions(&app, vec![colony("canary-org", "canary-repo", SessionStatus::Running)]).await;

    let body = text(router(&app).oneshot(get(PATH, Some(&app.api_token))).await.unwrap()).await;
    assert!(!body.contains("canary-org"), "a hashed org must not appear:\n{body}");
    assert!(!body.contains("canary-repo"), "a repository never appears at all:\n{body}");
    let hashed = body
        .lines()
        .find_map(|line| line.strip_prefix("colonizer_colonies{")?.split(' ').next())
        .expect("a colonies series");
    assert!(hashed.contains("org=\""), "the org is still a label, just a hash: {hashed}");
    // 24 hex characters, as the ADR's `hmac-sha256(key, name)[..12]` says.
    let value = hashed.split("org=\"").nth(1).unwrap().split('"').next().unwrap();
    assert_eq!(value.len(), 24, "{value}");
    assert!(value.chars().all(|c| c.is_ascii_hexdigit()), "{value}");

    // The key file is owner-only, and it is 32 random bytes.
    let key = std::fs::read(dir.0.join("data/observability/hash.key")).unwrap();
    assert_eq!(key.len(), 32);
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(dir.0.join("data/observability/hash.key"))
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o600, "the key is a secret: owner-only");

    // A hashed install with no key to hash with answers that it is temporarily unavailable, rather
    // than reporting every org as `unknown` — a real number that says nothing, and one a dashboard
    // would cache as though it were the install. It lives here rather than in a test of its own
    // because the key is process-wide, and a second test would race this one for it.
    let key_path = dir.0.join("data/observability/hash.key");
    std::fs::remove_file(&key_path).unwrap();
    std::fs::create_dir(&key_path).expect("a directory where the key belongs: it cannot be read or created");
    set_hash_key(None);
    let res = router(&app).oneshot(get(PATH, Some(&app.api_token))).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "hashed, with nowhere to hash with"
    );
    let body = text(res).await;
    assert!(!body.contains("unknown"), "and it says why rather than reporting one: {body}");
    set_hash_key(None);
    drop(dir);
}

#[test]
fn a_label_value_is_cut_to_the_safe_charset_and_the_length_cap() {
    assert_eq!(label("openrouter"), "openrouter");
    assert_eq!(label("anthropic/claude"), "anthropic_claude");
    assert_eq!(label("say \"hi\"\nnow\\then"), "say__hi__now_then");
    assert_eq!(label(&"a".repeat(200)).len(), MAX_LABEL);
    assert_eq!(label("").len(), 0);
}

#[test]
fn ten_thousand_orgs_cost_the_cap_plus_one_series() {
    let mut snapshot = seeded_snapshot();
    snapshot.colonies = (0..10_000)
        .map(|i| ("running".to_string(), format!("org-{i}"), "claude-code".to_string(), 1))
        .collect();
    let body = render(&snapshot, &plain());
    let series: Vec<&str> = body.lines().filter(|line| line.starts_with("colonizer_colonies{")).collect();
    assert_eq!(series.len(), SERIES_CAP + 1, "one `other` series past the cap, no more");
    assert!(series.iter().any(|line| line.contains("org=\"other\"")));
    // The folded series carries the whole overflow, not a sample of it.
    assert_eq!(
        value_of(&body, "colonizer_colonies", &[("org", "other")]),
        Some((10_000 - SERIES_CAP) as f64)
    );
}

// ---------------------------------------------------------------------------
// The histogram.
// ---------------------------------------------------------------------------

#[test]
fn the_histogram_buckets_are_cumulative_and_the_count_is_the_observation_total() {
    let histogram = Histogram::default();
    for ms in [50, 120, 200, 800, 3_000, 7_000, 45_000, 400_000] {
        histogram.observe(ms);
    }
    let snapshot = histogram.snapshot();
    assert_eq!(snapshot.count, 8);
    // The observation that landed above every bound is in no finite bucket, and is only counted by
    // `_count` and the `+Inf` bucket.
    let finite: u64 = snapshot.buckets.iter().sum();
    assert_eq!(finite, 7, "the 400s observation fell in no finite bucket");

    let mut family = HistogramFamily::new("colonizer_gateway_request_duration_seconds", "help");
    family.push("openrouter", snapshot);
    let body = family.render_into();

    let mut previous = 0.0;
    for (at, le) in LE_LABELS.iter().enumerate() {
        let got = value_of(&body, "colonizer_gateway_request_duration_seconds_bucket", &[("le", le)]).unwrap();
        assert!(got >= previous, "buckets must be cumulative: {got} < {previous} at le={le}");
        previous = got;
        assert_eq!(got, snapshot.buckets[..=at].iter().sum::<u64>() as f64, "bucket le={le}");
    }
    assert_eq!(previous, 7.0, "every observation but the largest");
    assert_eq!(
        value_of(&body, "colonizer_gateway_request_duration_seconds_bucket", &[("le", "+Inf")]),
        Some(8.0)
    );
    assert_eq!(
        value_of(&body, "colonizer_gateway_request_duration_seconds_count", &[]),
        Some(8.0)
    );
    let sum = value_of(&body, "colonizer_gateway_request_duration_seconds_sum", &[]).unwrap();
    assert!((sum - 456.17).abs() < 1e-9, "the sum is in seconds, not milliseconds: {sum}");
}

#[test]
fn a_histogram_folds_past_the_cap_by_adding_up() {
    let mut family = HistogramFamily::new("colonizer_gateway_request_duration_seconds", "help");
    let one = Histogram::default().snapshot();
    for i in 0..(SERIES_CAP + 10) {
        let h = Histogram::default();
        h.observe(1_000 * (i as u64 + 1));
        family.push(&format!("provider-{i}"), merge(one, h.snapshot()));
    }
    let body = family.render_into();
    let buckets = body
        .lines()
        .filter(|line| line.starts_with("colonizer_gateway_request_duration_seconds_bucket"))
        .count();
    assert_eq!(buckets, (SERIES_CAP + 1) * (LE_LABELS.len() + 1));
    let count = value_of(
        &body,
        "colonizer_gateway_request_duration_seconds_count",
        &[("provider", "other")],
    )
    .unwrap();
    assert_eq!(count, 10.0, "the folded series holds the ten providers past the cap");
    // ... and the first ten named series are untouched by the fold.
    assert_eq!(
        value_of(
            &body,
            "colonizer_gateway_request_duration_seconds_count",
            &[("provider", "provider-0")]
        ),
        Some(1.0)
    );
}

// ---------------------------------------------------------------------------
// Auth.
// ---------------------------------------------------------------------------

async fn token_for(app: &Shared, scope: &str, orgs: &[&str], repos: &[&str]) -> String {
    app.api_tokens
        .create(crate::api_tokens::NewToken {
            name: format!("metrics-{scope}"),
            scope: scope.into(),
            orgs: orgs.iter().map(|o| (*o).to_string()).collect(),
            repos: repos.iter().map(|r| (*r).to_string()).collect(),
            max_concurrent: None,
            budget_usd_per_day: None,
        })
        .await
        .expect("a token is created")
        .token
}

#[tokio::test]
async fn the_auth_matrix_is_the_documented_one() {
    let dir = root("auth");
    let off = app_without_metrics(&dir.0);
    let app = app_with_metrics(&dir.0, &[]).await;
    let read = token_for(&app, "read", &[], &[]).await;
    let operate = token_for(&app, "operate", &[], &[]).await;
    let launch = token_for(&app, "launch", &[], &[]).await;
    let org_limited = token_for(&app, "read", &["acme"], &[]).await;
    let repo_limited = token_for(&app, "read", &[], &["acme/web"]).await;
    let fleet = app.api_tokens.create_fleet_token("member").await.expect("a fleet token").0;

    // 404: the module is absent, or `prometheus` is off. An endpoint nobody configured does not
    // exist to be found — and it says so before any token does.
    let res = router(&off).oneshot(get(PATH, Some(&off.api_token))).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "no observability module at all");
    let switched_off = app_with_metrics(&dir.0, &[]).await;
    switched_off.modules.write().await.observability = Some(ModuleChoice {
        provider: crate::observability::settings::FILE.into(),
        enabled: false,
        settings: serde_json::Map::new(),
    });
    let res = router(&switched_off)
        .oneshot(get(PATH, Some(&switched_off.api_token)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "a disabled module");
    let prometheus_off = app_with_metrics(&dir.0, &[]).await;
    prometheus_off.modules.write().await.observability.as_mut().unwrap().settings = serde_json::Map::new();
    let res = router(&prometheus_off)
        .oneshot(get(PATH, Some(&prometheus_off.api_token)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND, "prometheus switched off");

    // 401: no token, and a token that is not one.
    let res = router(&app).oneshot(get(PATH, None)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let body = text(res).await;
    assert!(
        !body.contains("<html"),
        "a scraper gets a plain 401, not the sign-in page: {body}"
    );
    let res = router(&app).oneshot(get(PATH, Some("col_nope"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 200: the owner, and an unlimited token at any scope at or above read.
    let res = router(&app).oneshot(get(PATH, Some(&app.api_token))).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get(header::CONTENT_TYPE).unwrap(), CONTENT_TYPE);
    for (who, token) in [("read", &read), ("operate", &operate), ("launch", &launch)] {
        let res = router(&app).oneshot(get(PATH, Some(token))).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "an unlimited {who} token scrapes");
    }

    // 403: a paired phone. `host_guard` authenticates it as the owner and lets it make any GET, so
    // the handler is what refuses it: the install's numbers are the owner's, not a device's.
    let phone = app.phones.add("test-phone").expect("a phone pairs");
    let res = router(&app).oneshot(get_with_cookie(PATH, &phone)).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN, "a paired phone is refused");

    // 403: a token bounded to some orgs or repos, and a fleet token. A filtered page would hide the
    // series that are out of reach, so the endpoint refuses instead.
    for (who, token) in [
        ("org-limited", &org_limited),
        ("repo-limited", &repo_limited),
        ("fleet", &fleet),
    ] {
        let res = router(&app).oneshot(get(PATH, Some(token))).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "a {who} token is refused");
    }
    drop(dir);
}

// ---------------------------------------------------------------------------
// The counters.
// ---------------------------------------------------------------------------

/// A scrape reads the gateway; it does not write to it. The old path called `load`/`usage` per
/// provider, each of which inserts a zeroed entry, so scraping an install persisted entries for
/// providers that had never carried a request. Here the app is built over a provider that has
/// counters but no live stats — the asymmetric case that used to grow a twin.
#[tokio::test]
async fn a_scrape_leaves_the_gateway_exactly_as_it_found_it() {
    let dir = root("readonly");
    let tally = dir.0.join("data/provider-usage.json");
    std::fs::write(&tally, r#"{"openrouter":{"requests":5,"failures":1}}"#).unwrap();
    let app = app_with_metrics(&dir.0, &[]).await;

    let body = text(router(&app).oneshot(get(PATH, Some(&app.api_token))).await.unwrap()).await;
    assert!(body.contains("provider=\"openrouter\""), "the provider is reported: {body}");

    // Scrape again, then flush: the file is the whole usage map, so an entry a scrape created
    // would be in it.
    let _ = text(router(&app).oneshot(get(PATH, Some(&app.api_token))).await.unwrap()).await;
    app.gateway.flush_usage();
    let persisted: serde_json::Value = serde_json::from_slice(&std::fs::read(&tally).unwrap()).unwrap();
    let ids: Vec<&String> = persisted.as_object().expect("a map of provider tallies").keys().collect();
    assert_eq!(ids, vec!["openrouter"], "a scrape created a provider entry: {ids:?}");
    assert_eq!(persisted["openrouter"]["requests"], 5, "the tally is untouched");
    assert_eq!(persisted["openrouter"]["failures"], 1);
    drop(dir);
}

/// The attention flag is free-form JSON, so the reasons a series may carry are a closed set. Every
/// reason the install writes today keeps its own label, and anything else — a sentence, a file
/// path, a half-finished edit — is counted under `other` and never named.
#[tokio::test]
async fn an_attention_reason_carrying_free_text_is_counted_and_never_named() {
    let dir = root("reasons");
    let app = app_with_metrics(&dir.0, &[]).await;
    let mut sentence = colony("acme", "acme/web", SessionStatus::Failed);
    sentence.attention = Some(json!({"reason": "DROP TABLE sessions; -- /home/me/notes.md"}));
    let mut known = colony("acme", "acme/web", SessionStatus::Failed);
    known.attention = Some(json!({"reason": "stalled"}));
    seed_sessions(&app, vec![sentence, known]).await;

    let body = text(router(&app).oneshot(get(PATH, Some(&app.api_token))).await.unwrap()).await;
    assert!(!body.contains("notes.md"), "free text reached a label:\n{body}");
    assert_eq!(value_of(&body, "colonizer_attention", &[("reason", "stalled")]), Some(1.0));
    assert_eq!(value_of(&body, "colonizer_attention", &[("reason", "other")]), Some(1.0));
    drop(dir);
}

#[test]
fn the_closed_set_of_attention_reasons_keeps_its_own_and_folds_the_rest() {
    for reason in ATTENTION_REASONS {
        assert_eq!(attention_reason(reason), reason, "{reason} is a reason this build writes");
    }
    for free_text in [
        "DROP TABLE sessions; -- /home/me/notes.md",
        "the provider said no, again, politely",
        "Stalled",
        "",
        "stalled ",
    ] {
        assert_eq!(attention_reason(free_text), "other", "{free_text:?} must not become a series");
    }
}

#[test]
fn the_colony_counters_seed_from_the_log_and_count_live_lines_once() {
    let dir = root("counters");
    let live = dir.0.join("data/activity.jsonl");
    let write = |kind: &str| {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&live).unwrap();
        use std::io::Write as _;
        writeln!(f, "{{\"seq\":1,\"kind\":\"{kind}\",\"actor\":\"colony\"}}").unwrap();
    };
    write(LAUNCH);
    write(LAUNCH);
    write("outcome.merged");
    write("outcome.finished_but_unknown");
    write("colony.stop");

    let (started, finished) = seed_from_log(&dir.0.join("data"));
    assert_eq!(started, 2, "only `colony.launch` is a start");
    assert_eq!(finished[OUTCOMES.iter().position(|o| *o == "merged").unwrap()], 1);
    assert_eq!(finished.iter().sum::<u64>(), 1, "an unknown outcome suffix is not counted");

    // A live line adds exactly once, and only after the seed has published.
    COLONIES_STARTED.store(started, Ordering::Release);
    SEEDED.store(true, Ordering::Release);
    let before = colony_counters().0;
    on_activity(LAUNCH);
    on_activity("outcome.closed");
    on_activity("colony.launch.typo");
    let (after, finished_after) = colony_counters();
    assert_eq!(after, before + 1);
    assert_eq!(
        finished_after[OUTCOMES.iter().position(|o| *o == "closed").unwrap()],
        finished[OUTCOMES.iter().position(|o| *o == "closed").unwrap()] + 1
    );
    drop(dir);
}
