//! Paged event-log reads and the run summary (issue #1210).

use super::*;
use crate::store::{LocalDirStore, local_session_dir};

/// A run of `count` events: a `user_message` every `turn` events, a `turn_end` (with cumulative
/// usage) just before the next one, deltas between.
fn run(count: u64, turn: u64) -> String {
    let mut out = String::new();
    for seq in 1..=count {
        let line = if seq % turn == 1 {
            json!({"seq": seq, "type": "user_message", "id": format!("u{seq}"), "text": "go"})
        } else if seq % turn == 0 {
            json!({"seq": seq, "type": "turn_end", "is_error": false, "result": null, "cost_usd": seq as f64 / 100.0,
                   "duration_ms": 5, "model_usage": {"m": {"input_tokens": seq, "output_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0}}})
        } else {
            json!({"seq": seq, "type": "assistant_text_delta", "message_id": "a", "block_index": 0, "delta": "x"})
        };
        out.push_str(&line.to_string());
        out.push('\n');
    }
    out
}

fn fixture(tag: &str) -> (LocalDirStore, std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("colonizer-history-{tag}-{}", crate::util::short_id()));
    let dir = local_session_dir(&root, "s1");
    std::fs::create_dir_all(&dir).unwrap();
    (LocalDirStore::new(&root), root, dir)
}

fn seqs(page: &Page) -> Vec<u64> {
    page.events.iter().map(|e| e["seq"].as_u64().unwrap()).collect()
}

#[tokio::test]
async fn the_newest_page_is_the_last_events_starting_on_a_turn() {
    let (store, root, dir) = fixture("tail");
    std::fs::write(dir.join("events.jsonl"), run(1000, 10)).unwrap();
    let page = page(&store, "s1", 1, None, 25, false).await.unwrap();
    let seqs = seqs(&page);
    // At least 25 events, extended back to the turn start (seq 971), never past the end.
    assert_eq!(seqs.first(), Some(&971));
    assert_eq!(seqs.last(), Some(&1000));
    assert!(page.has_more);
    assert_eq!(page.oldest.seq, 971);
    // The baseline is the totals as of the turn that ended just before the page.
    assert_eq!(page.baseline_usage.as_ref().unwrap()["m"]["input_tokens"], 970);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn pages_walk_backwards_across_segments_without_gaps_or_repeats() {
    let (store, root, dir) = fixture("walk");
    std::fs::write(dir.join("events-1.jsonl"), run(120, 10)).unwrap();
    std::fs::write(dir.join("events-2.jsonl"), run(75, 10)).unwrap();
    std::fs::write(dir.join("events.jsonl"), run(300, 10)).unwrap();
    let current = 3;
    let mut walked: Vec<(u64, u64)> = Vec::new();
    let mut cursor: Option<Before> = None;
    let mut pages = 0;
    loop {
        let p = page(&store, "s1", current, cursor, 50, true).await.unwrap();
        pages += 1;
        // Newest page first, so each page goes in front of what is already walked.
        let epoch_of = |seq: u64, first: u64| (first, seq);
        let mut chunk: Vec<(u64, u64)> = Vec::new();
        let mut at = p.oldest.epoch;
        let mut prev = 0;
        for s in seqs(&p) {
            // seq restarts at 1 in each run, so a drop means the page crossed into the next run.
            if s < prev {
                at += 1;
            }
            prev = s;
            chunk.push(epoch_of(s, at));
        }
        chunk.extend(walked);
        walked = chunk;
        if !p.has_more {
            break;
        }
        cursor = Some(Before {
            epoch: p.oldest.epoch,
            seq: p.oldest.seq,
            offset: Some(p.oldest.offset),
        });
        assert!(pages < 20, "the walk must end");
    }
    let expected: Vec<(u64, u64)> = (1..=120)
        .map(|s| (1, s))
        .chain((1..=75).map(|s| (2, s)))
        .chain((1..=300).map(|s| (3, s)))
        .collect();
    assert_eq!(walked, expected, "every event of every run exactly once, in order");
    assert!(pages > 3);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_socket_page_stays_in_this_run_and_says_more_is_behind_it() {
    let (store, root, dir) = fixture("run");
    std::fs::write(dir.join("events-1.jsonl"), run(40, 10)).unwrap();
    std::fs::write(dir.join("events.jsonl"), run(5, 10)).unwrap();
    let page = page(&store, "s1", 2, None, 50, false).await.unwrap();
    assert_eq!(seqs(&page), vec![1, 2, 3, 4, 5]);
    assert!(page.has_more, "the earlier run is behind it");
    assert_eq!(page.oldest.epoch, 2);
    // The first page of the older run continues from there.
    let older = super::page(
        &store,
        "s1",
        2,
        Some(Before {
            epoch: 2,
            seq: 1,
            offset: Some(0),
        }),
        50,
        true,
    )
    .await
    .unwrap();
    assert_eq!(older.oldest.epoch, 1);
    assert_eq!(seqs(&older).last(), Some(&40));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_stale_offset_falls_back_to_reading_by_seq() {
    let (store, root, dir) = fixture("stale");
    std::fs::write(dir.join("events.jsonl"), run(100, 10)).unwrap();
    // An offset in the middle of a line, as a rewritten log would leave.
    let before = Before {
        epoch: 1,
        seq: 51,
        offset: Some(7),
    };
    let page = page(&store, "s1", 1, Some(before), 20, true).await.unwrap();
    assert_eq!(seqs(&page).last(), Some(&50));
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_line_bigger_than_the_read_window_still_pages() {
    let (store, root, dir) = fixture("big");
    let mut log = run(10, 5);
    let big = json!({"seq": 11, "type": "tool_result", "tool_call_id": "t", "output": "y".repeat(600 * 1024), "is_error": false});
    log.push_str(&format!("{big}\n"));
    log.push_str(&run(12, 5).lines().skip(11).map(|l| format!("{l}\n")).collect::<String>());
    std::fs::write(dir.join("events.jsonl"), log).unwrap();
    let page = page(&store, "s1", 1, None, 4, false).await.unwrap();
    assert!(seqs(&page).contains(&11) || page.oldest.seq > 11);
    let all = super::page(&store, "s1", 1, None, MAX_LIMIT, false).await.unwrap();
    assert_eq!(seqs(&all), (1..=12).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_summary_keeps_settlers_cost_and_the_brief_of_the_whole_run() {
    let mut log = String::new();
    let mut push = |v: Value| {
        log.push_str(&format!("{v}\n"));
    };
    let a = json!({"id": "t1", "name": "Explore", "description": "look"});
    let b = json!({"id": "t2", "name": "Explore", "description": "look again"});
    push(json!({"seq": 1, "type": "user_message", "id": "initial", "text": "the brief"}));
    push(json!({"seq": 2, "type": "model_changed", "model": "opus", "previous": null}));
    push(
        json!({"seq": 3, "type": "tool_call", "message_id": "m", "tool_call_id": "c1", "name": "Read", "input": {}, "agent": a}),
    );
    push(json!({"seq": 4, "type": "tool_result", "tool_call_id": "c1", "output": "", "is_error": true, "agent": a}));
    push(
        json!({"seq": 5, "type": "tool_call", "message_id": "m", "tool_call_id": "c2", "name": "Grep", "input": {}, "agent": b}),
    );
    push(json!({"seq": 6, "type": "tool_result", "tool_call_id": "c2", "output": "", "is_error": false, "agent": b}));
    push(json!({"seq": 7, "type": "turn_end", "is_error": false, "result": null, "cost_usd": 1.5, "duration_ms": 1}));
    push(json!({"seq": 8, "type": "status", "state": "idle"}));
    let mut summary = Summary::from_log(log.as_bytes());
    assert_eq!(summary.turns, 1);
    assert_eq!(summary.cost_usd, Some(1.5));
    assert_eq!(summary.model.as_deref(), Some("opus"));
    assert_eq!(summary.brief.as_ref().unwrap()["text"], "the brief");
    assert_eq!(summary.settlers.len(), 2);
    assert_eq!((summary.settlers[0].steps, summary.settlers[0].errors), (1, 1));
    assert_eq!(summary.settlers[1].last_tool.as_deref(), Some("Grep"));
    // A live broadcast that overlaps the log is not counted twice.
    summary.apply(&json!({"seq": 7, "type": "turn_end", "cost_usd": 9.0}));
    assert_eq!(summary.turns, 1);
    summary.apply(&json!({"seq": 9, "type": "turn_end", "is_error": false, "cost_usd": 2.0}));
    assert_eq!((summary.turns, summary.cost_usd), (2, Some(2.0)));
}

/// The events socket: a `limit` first paint carries only the newest page, the `history` frame says
/// what is behind it and carries the summary; the plain GET pages further back.
#[tokio::test]
async fn the_socket_first_paint_is_only_the_last_page() {
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderValue};
    use tower::ServiceExt as _;
    let (app, root) = crate::sessions::tests::app_with_colony("abc", SessionStatus::Running).await;
    std::fs::write(app.session_dir("abc").join("events.jsonl"), run(2000, 10)).unwrap();
    let token = app.phones.add("iPhone").unwrap();
    let cookie = format!("{}={token}", crate::auth::COOKIE_NAME);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = crate::server::router(&app);
    let server = tokio::spawn(async move { axum::serve(listener, served).await.unwrap() });
    let mut req = format!("ws://{addr}/api/sessions/abc/events?since=0&epoch=0&limit=30")
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", HeaderValue::from_str(&cookie).unwrap());
    req.headers_mut()
        .insert("origin", HeaderValue::from_str(&format!("http://{addr}")).unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let mut events = Vec::new();
    let mut history = None;
    while let Some(Ok(message)) = socket.next().await {
        let Message::Text(text) = message else { continue };
        let frame: Value = serde_json::from_str(text.as_str()).unwrap();
        match frame["type"].as_str() {
            Some("replay_done") => break,
            Some("history") => history = Some(frame),
            _ if frame["seq"].is_u64() => events.push(frame["seq"].as_u64().unwrap()),
            _ => {}
        }
    }
    let history = history.expect("a limit first paint carries a history frame");
    assert_eq!(events.first(), Some(&1971), "the last 30 events, which begin on a turn");
    assert_eq!(events.last(), Some(&2000));
    assert!(events.len() < 40, "only the last page was sent: {}", events.len());
    assert_eq!(history["has_more"], true);
    assert_eq!(history["oldest_seq"], 1971);
    assert_eq!(history["summary"]["turns"], 200, "the summary covers what was not sent");
    assert_eq!(history["summary"]["brief"], Value::Null, "no `initial` message in this log");
    assert_eq!(history["baseline_usage"]["m"]["input_tokens"], 1970);

    // Without a limit the socket replays the whole run, as before.
    let mut req = format!("ws://{addr}/api/sessions/abc/events").into_client_request().unwrap();
    req.headers_mut().insert("cookie", HeaderValue::from_str(&cookie).unwrap());
    req.headers_mut()
        .insert("origin", HeaderValue::from_str(&format!("http://{addr}")).unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let mut all = 0;
    while let Some(Ok(message)) = socket.next().await {
        let Message::Text(text) = message else { continue };
        let frame: Value = serde_json::from_str(text.as_str()).unwrap();
        if frame["type"] == "replay_done" {
            break;
        }
        all += usize::from(frame["seq"].is_u64());
    }
    assert_eq!(all, 2000);

    // The plain GET pages back from the cursor the history frame named.
    let get = |query: String| {
        let mut req = axum::http::Request::builder()
            .uri(format!("/api/sessions/abc/events{query}"))
            .header("cookie", cookie.clone())
            .header("host", addr.to_string())
            .header("origin", format!("http://{addr}"))
            .body(axum::body::Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 1))));
        async { crate::server::router(&app).oneshot(req).await.unwrap() }
    };
    let res = get(format!("?before=1971&epoch=1&offset={}&limit=30", history["offset"])).await;
    assert_eq!(res.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    let last = body["events"].as_array().unwrap().last().unwrap()["seq"].clone();
    assert_eq!(last, 1970);
    assert_eq!(body["has_more"], true);
    let latest = get("?limit=5".into()).await;
    let latest: Value = serde_json::from_slice(&axum::body::to_bytes(latest.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(latest["events"].as_array().unwrap().last().unwrap()["seq"], 2000);
    server.abort();
    let _ = std::fs::remove_dir_all(root);
}
