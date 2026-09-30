//! The owner's history view against a real owner: members push over HTTP with their fleet tokens,
//! then the owner's cockpit reads, filters, pages and prunes what arrived.

use super::*;
use crate::fleet_export::Origin;
use crate::fleet_members::FleetStore;
use crate::sessions::tests::colony;

/// A temp root that removes itself.
struct TempRoot(PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> TempRoot {
    let root = TempRoot(std::env::temp_dir().join(format!("colonizer-fleet-history-{tag}-{}", util::short_id())));
    std::fs::create_dir_all(root.0.join("config")).unwrap();
    root
}

fn sha(bytes: &[u8]) -> String {
    util::hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// The log a colony `id` carries.
fn log_of(id: &str) -> String {
    format!("{{\"colony\":\"{id}\"}}\n")
}

/// Pushes one finished colony, as the member's drain would: its log first, then its row.
#[allow(clippy::too_many_arguments)]
async fn push(url: &str, token: &str, host: &str, id: &str, repo: &str, status: SessionStatus, days_ago: i64, cost: Option<f64>) {
    let mut s = colony("acme", status);
    s.id = id.into();
    s.repo = repo.into();
    s.updated_at = Utc::now() - chrono::Duration::days(days_ago);
    s.created_at = s.updated_at - chrono::Duration::hours(1);
    s.cost_usd = cost;
    let record = ImportedSession::of(
        &Origin {
            host: host.into(),
            name: host.into(),
        },
        &s,
    );
    let log = log_of(id);
    let key = sha(log.as_bytes());
    let client = reqwest::Client::new();
    let res = client
        .put(format!("{url}/api/fleet/peer/payloads/{key}"))
        .bearer_auth(token)
        .body(log.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::NO_CONTENT);
    let answer: Value = client
        .post(format!("{url}/api/fleet/peer/rows"))
        .bearer_auth(token)
        .json(&json!({"rows": [{"id": record.id, "record": record,
            "payloads": [{"name": "events.jsonl", "sha256": key, "bytes": log.len()}]}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(answer["accepted"], json!([record.id]), "{answer}");
}

struct Owner {
    _root: TempRoot,
    app: Shared,
    url: String,
    alpha: (String, String),
    beta: (String, String),
}

/// An owner with two members, alpha (three colonies on acme/one) and beta (two on acme/two).
async fn owner() -> Owner {
    let root = temp("owner");
    let app = crate::tests::test_app(&root.0);
    let alpha = FleetStore::add_member_for_tests(&app, "alpha").await;
    let beta = FleetStore::add_member_for_tests(&app, "beta").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let served = app.clone();
    tokio::spawn(async move { axum::serve(listener, crate::server::router(&served)).await.unwrap() });

    push(&url, &alpha.1, "hostA", "a1", "acme/one", SessionStatus::Merged, 1, Some(1.0)).await;
    push(&url, &alpha.1, "hostA", "a2", "acme/one", SessionStatus::Merged, 3, Some(2.0)).await;
    push(&url, &alpha.1, "hostA", "a3", "acme/one", SessionStatus::Failed, 5, None).await;
    push(
        &url,
        &beta.1,
        "hostB",
        "b1",
        "acme/two",
        SessionStatus::PrOpened,
        2,
        Some(0.5),
    )
    .await;
    push(&url, &beta.1, "hostB", "b2", "acme/two", SessionStatus::Merged, 4, None).await;
    Owner {
        _root: root,
        app,
        url,
        alpha,
        beta,
    }
}

async fn get(o: &Owner, path_and_query: &str) -> (reqwest::StatusCode, Value) {
    let res = reqwest::Client::new()
        .get(format!("{}{path_and_query}", o.url))
        .bearer_auth(&o.app.api_token)
        .send()
        .await
        .unwrap();
    let status = res.status();
    (status, res.json().await.unwrap_or(Value::Null))
}

fn ids(page: &Value) -> Vec<String> {
    page["colonies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["record"]["original_id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn ingested_rows_are_listed_filtered_paged_and_totalled() {
    let o = owner().await;
    let (status, all) = get(&o, "/api/fleet/history").await;
    assert_eq!(status, reqwest::StatusCode::OK, "{all}");
    assert_eq!(ids(&all), ["a1", "b1", "a2", "b2", "a3"], "newest finish first");
    assert_eq!(all["next_cursor"], Value::Null);
    let first = &all["colonies"][0];
    assert_eq!(first["member_id"], o.alpha.0);
    assert_eq!(first["member_name"], "alpha");
    assert_eq!(first["member_removed"], false);
    assert_eq!(first["id"], "hostA:a1");
    assert_eq!(first["key"], format!("{}/hostA:a1", o.alpha.0));
    assert_eq!(all["repos"], json!(["acme/one", "acme/two"]));
    assert_eq!(all["retention_days"], DEFAULT_RETENTION_DAYS);

    // Totals: everything, per member, per repo.
    assert_eq!(all["stats"]["total"], json!({"colonies": 5, "merged": 3, "cost_usd": 3.5}));
    let alpha = all["stats"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "alpha")
        .unwrap();
    assert_eq!((alpha["colonies"].as_u64(), alpha["merged"].as_u64()), (Some(3), Some(2)));
    assert_eq!(alpha["cost_usd"], 3.0);
    let two = all["stats"]["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["repo"] == "acme/two")
        .unwrap();
    assert_eq!(two["colonies"], 2);
    assert_eq!(two["cost_usd"], 0.5);

    // Filters.
    let (_, beta) = get(&o, &format!("/api/fleet/history?member={}", o.beta.0)).await;
    assert_eq!(ids(&beta), ["b1", "b2"]);
    assert_eq!(beta["stats"]["total"]["colonies"], 2, "totals follow the filter");
    let (_, repo) = get(&o, "/api/fleet/history?repo=acme/one").await;
    assert_eq!(ids(&repo), ["a1", "a2", "a3"]);
    let (_, merged) = get(&o, "/api/fleet/history?status=merged").await;
    assert_eq!(ids(&merged), ["a1", "a2", "b2"]);
    let since = (Utc::now() - chrono::Duration::days(3) - chrono::Duration::hours(1)).to_rfc3339();
    let until = (Utc::now() - chrono::Duration::days(1) - chrono::Duration::hours(1)).to_rfc3339();
    let (_, range) = get(
        &o,
        &format!(
            "/api/fleet/history?since={}&until={}",
            urlencoding(&since),
            urlencoding(&until)
        ),
    )
    .await;
    assert_eq!(ids(&range), ["b1", "a2"]);
    let (status, _) = get(&o, "/api/fleet/history?since=yesterday").await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);

    // Pages: walking `next_cursor` visits every entry once, and ends at null.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let q = match &cursor {
            Some(c) => format!("/api/fleet/history?limit=2&cursor={}", urlencoding(c)),
            None => "/api/fleet/history?limit=2".into(),
        };
        let (status, page) = get(&o, &q).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{page}");
        assert!(page["colonies"].as_array().unwrap().len() <= 2);
        seen.extend(ids(&page));
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(seen, ["a1", "b1", "a2", "b2", "a3"]);
    let (status, _) = get(&o, "/api/fleet/history?cursor=nobody/none").await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
}

fn urlencoding(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn one_colonys_record_and_its_logs_are_readable() {
    let o = owner().await;
    let base = format!("/api/fleet/history/{}/hostA:a2", o.alpha.0);
    let (status, detail) = get(&o, &base).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{detail}");
    assert_eq!(detail["record"]["repo"], "acme/one");
    assert_eq!(detail["record"]["cost_usd"], 2.0);
    assert_eq!(detail["logs"][0]["name"], "events.jsonl");
    assert_eq!(detail["logs"][0]["stored"], true);

    let res = reqwest::Client::new()
        .get(format!("{}{base}/logs/events.jsonl", o.url))
        .bearer_auth(&o.app.api_token)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    assert_eq!(res.text().await.unwrap(), log_of("a2"));

    for missing in [
        format!("{base}/logs/harness.jsonl"),
        format!("/api/fleet/history/{}/hostA:nope", o.alpha.0),
        "/api/fleet/history/mem_nobody/hostA:a2".to_string(),
    ] {
        let (status, _) = get(&o, &missing).await;
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{missing}");
    }
}

#[tokio::test]
async fn only_the_owner_reads_fleet_history() {
    let o = owner().await;
    let read = o
        .app
        .api_tokens
        .create(crate::api_tokens::NewToken {
            name: "watcher".into(),
            scope: "read".into(),
            orgs: Vec::new(),
            repos: Vec::new(),
            max_concurrent: None,
            budget_usd_per_day: None,
        })
        .await
        .unwrap()
        .token;
    let paths = [
        "/api/fleet/history".to_string(),
        format!("/api/fleet/history/{}/hostA:a1", o.alpha.0),
        format!("/api/fleet/history/{}/hostA:a1/logs/events.jsonl", o.alpha.0),
    ];
    let client = reqwest::Client::new();
    for path in &paths {
        for token in [&read, &o.alpha.1] {
            let res = client
                .get(format!("{}{path}", o.url))
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), reqwest::StatusCode::FORBIDDEN, "{path}");
        }
        let res = client.get(format!("{}{path}", o.url)).send().await.unwrap();
        assert_eq!(res.status(), reqwest::StatusCode::UNAUTHORIZED, "{path}");
    }
}

#[tokio::test]
async fn a_removed_members_history_stays_readable_and_is_marked_removed() {
    let o = owner().await;
    FleetStore::remove_member_for_tests(&o.app, &o.beta.0).await;
    let (_, all) = get(&o, "/api/fleet/history").await;
    assert_eq!(ids(&all), ["a1", "b1", "a2", "b2", "a3"]);
    let b1 = &all["colonies"][1];
    assert_eq!(b1["member_removed"], true);
    assert_eq!(b1["member_name"], "beta", "the name survives in member.json");
    assert_eq!(all["colonies"][0]["member_removed"], false);
    let member = all["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == o.beta.0)
        .unwrap();
    assert_eq!(member["removed"], true);
    let (status, detail) = get(&o, &format!("/api/fleet/history/{}/hostB:b1", o.beta.0)).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(detail["member_removed"], true);
}

#[tokio::test]
async fn retention_prunes_old_rows_and_the_payloads_only_they_held() {
    let root = temp("retention");
    let data = root.0.join("data");
    let dir = data.join(INGEST_DIR).join("mem_1");
    std::fs::create_dir_all(dir.join("payloads")).unwrap();
    let now = Utc::now();
    let mut old = colony("acme", SessionStatus::Merged);
    old.id = "old".into();
    let mut new = colony("acme", SessionStatus::Merged);
    new.id = "new".into();
    let origin = Origin {
        host: "h".into(),
        name: "h".into(),
    };
    let (old_sha, new_sha, orphan_old, orphan_fresh) = ("1".repeat(64), "2".repeat(64), "3".repeat(64), "4".repeat(64));
    let rows = json!({
        "h:old": {"record": ImportedSession::of(&origin, &old), "received_at": now - chrono::Duration::days(100),
                  "payloads": [{"name": "events.jsonl", "sha256": old_sha, "bytes": 1}]},
        "h:new": {"record": ImportedSession::of(&origin, &new), "received_at": now - chrono::Duration::days(10),
                  "payloads": [{"name": "events.jsonl", "sha256": new_sha, "bytes": 1}]},
    });
    std::fs::write(dir.join(ROWS_FILE), serde_json::to_vec(&rows).unwrap()).unwrap();
    let long_ago = std::time::SystemTime::from(now - chrono::Duration::days(120));
    for (name, aged) in [
        (&old_sha, true),
        (&new_sha, true),
        (&orphan_old, true),
        (&orphan_fresh, false),
    ] {
        let path = dir.join("payloads").join(name);
        std::fs::write(&path, b"x").unwrap();
        if aged {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
    }

    assert_eq!(prune(&data, 0, now).await, Pruned::default(), "0 keeps everything");
    let pruned = prune(&data, 90, now).await;
    assert_eq!(pruned, Pruned { rows: 1, payloads: 2 });
    assert_eq!(read_rows(&dir).keys().collect::<Vec<_>>(), ["h:new"]);
    assert!(!dir.join("payloads").join(&old_sha).exists(), "only the pruned row held it");
    assert!(!dir.join("payloads").join(&orphan_old).exists(), "an old orphan goes");
    assert!(dir.join("payloads").join(&new_sha).exists(), "a kept row's payload stays");
    assert!(
        dir.join("payloads").join(&orphan_fresh).exists(),
        "a fresh upload waits for its row"
    );

    // Everything past the window: the member directory goes with it.
    let later = now + chrono::Duration::days(200);
    let pruned = prune(&data, 90, later).await;
    assert_eq!(pruned.rows, 1);
    assert!(!dir.exists());

    assert_eq!(parse_retention_days(None), DEFAULT_RETENTION_DAYS);
    assert_eq!(parse_retention_days(Some("30")), 30);
    assert_eq!(parse_retention_days(Some("0")), 0);
    assert_eq!(parse_retention_days(Some("soon")), DEFAULT_RETENTION_DAYS);
}
