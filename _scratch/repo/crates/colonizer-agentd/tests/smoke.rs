//! The no-KVM smoke test: the boot path of a colony, minus the microVM. It boots the real
//! colonizer-agentd binary as a plain host process from a `session.json`, on loopback, against a
//! stub agent runner, and asserts what actually happened end to end (docs/protocol.md §2–§3): the
//! bearer-token wall, health, the runner's JSONL arriving stamped with `seq`/`ts`, messages and
//! answers reaching the runner's stdin, gap-free replay from `since`, and clean shutdown. It needs
//! no /dev/kvm, no network and no credentials, so `cargo test --workspace` runs it everywhere —
//! stock CI included.
//!
//! What it deliberately does not cover is everything a real colony adds around agentd: the `msb`
//! boot itself, the session directory the mothership writes (plugin mounts, boot.sh, mesh key) and
//! the real runner. `scripts/build-agentd.sh --smoke` covers the VM part on a machine with KVM.

use chrono::DateTime;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error as WsError, Message, client::IntoClientRequest, http::HeaderValue},
};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const TOKEN: &str = "smoke-token";

// A stub agent module that speaks the documented runner contract: status on every state change, a
// turn per user_message, exactly one question asked through the `question` event and answered
// through the `answer` command, and a graceful exit on `shutdown`.
const STUB_RUNNER: &str = r#"
import json, sys

def emit(**event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()

emit(type="status", state="idle")
asked = False
for line in sys.stdin:
    command = json.loads(line)
    kind = command.get("type")
    if kind == "user_message":
        emit(type="user_message", id=command["id"], text=command["text"])
        emit(type="status", state="working")
        emit(type="assistant_text", message_id="m-" + command["id"], block_index=0, text="echo: " + command["text"])
        if asked:
            emit(type="turn_end", is_error=False, result="echo: " + command["text"], cost_usd=0.0, duration_ms=1)
            emit(type="status", state="idle")
        else:
            asked = True
            emit(type="question", question_id="q-smoke", message_id="m-" + command["id"], questions=[{
                "question": "Ship it?", "header": "Confirm", "multi_select": False,
                "options": [{"label": "Yes", "description": "publish"}, {"label": "No", "description": "wait"}]}])
            emit(type="status", state="waiting_for_answer")
    elif kind == "answer":
        emit(type="question_answered", question_id=command["question_id"], answers=command["answers"], response=command["response"])
        emit(type="turn_end", is_error=False, result="answered", cost_usd=0.0, duration_ms=1)
        emit(type="status", state="idle")
    elif kind == "shutdown":
        emit(type="status", state="exited")
        sys.exit(0)
"#;

struct Daemon {
    child: Child,
    port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!("colonizer-agentd-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(dir.join("workspace")).unwrap();
    dir
}

/// Spawns the real binary from a session.json, the way a colony boots it, and waits for /v1/health.
async fn start(dir: &Path, initial_prompt: &str) -> Daemon {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(dir.join("runner.py"), STUB_RUNNER).unwrap();
    std::fs::write(dir.join("token"), format!("{TOKEN}\n")).unwrap();
    let config = json!({
        "session_id": "smoke",
        "workspace": dir.join("workspace"),
        "listen": format!("127.0.0.1:{port}"),
        "agent": {"module": "smoke", "command": ["python3", dir.join("runner.py")], "env": {}},
        "initial_prompt": initial_prompt,
    });
    std::fs::write(dir.join("session.json"), config.to_string()).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_colonizer-agentd"))
        .arg("--config")
        .arg(dir.join("session.json"))
        .arg("--token-file")
        .arg(dir.join("token"))
        .arg(format!("--state-dir={}", dir.join("state").display()))
        .env("HOME", dir)
        .spawn()
        .unwrap();
    let daemon = Daemon { child, port };
    for _ in 0..100 {
        if let Ok((200, _)) = http(port, "GET", "/v1/health", Some(TOKEN)).await {
            return daemon;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("colonizer-agentd did not become healthy");
}

async fn http(port: u16, method: &str, path: &str, token: Option<&str>) -> std::io::Result<(u16, String)> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let request = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Length: 0\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    let status = response.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

async fn ws(port: u16, path: &str, token: Option<&str>) -> Result<Ws, WsError> {
    let mut request = format!("ws://127.0.0.1:{port}{path}").into_client_request().unwrap();
    if let Some(token) = token {
        request
            .headers_mut()
            .insert("authorization", HeaderValue::from_str(&format!("Bearer {token}")).unwrap());
    }
    connect_async(request).await.map(|(stream, _)| stream)
}

async fn collect_until(ws: &mut Ws, done: impl Fn(&Value) -> bool) -> Vec<Value> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let message = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("timed out; events so far: {seen:#?}"))
            .expect("event stream ended")
            .expect("websocket error");
        if let Message::Text(text) = message {
            let event: Value = serde_json::from_str(&text).unwrap();
            let finished = done(&event);
            seen.push(event);
            if finished {
                return seen;
            }
        }
    }
}

fn has(events: &[Value], check: impl Fn(&Value) -> bool) -> bool {
    events.iter().any(check)
}

/// Asserts a stream of events carries agentd's stamping: a gap-free `seq` (starting at 1) and an
/// RFC 3339 UTC `ts` on every event.
fn assert_stamped(events: &[Value]) {
    for pair in events.windows(2) {
        assert_eq!(
            pair[1]["seq"].as_u64().unwrap(),
            pair[0]["seq"].as_u64().unwrap() + 1,
            "seq must be gap-free and monotonic: {events:#?}"
        );
    }
    for event in events {
        let ts = event["ts"]
            .as_str()
            .unwrap_or_else(|| panic!("every event carries a ts: {event}"));
        DateTime::parse_from_rfc3339(ts).unwrap_or_else(|e| panic!("ts must be RFC 3339: {ts} ({e})"));
        assert!(ts.ends_with('Z'), "ts must be UTC: {ts}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_to_health_events_message_replay_and_clean_shutdown() {
    let dir = scratch("smoke");
    let mut daemon = start(&dir, "boot check").await;
    let port = daemon.port;

    // The token wall comes first: nothing is answerable without it, including the shutdown POST.
    assert_eq!(http(port, "GET", "/v1/health", None).await.unwrap().0, 401);
    assert_eq!(http(port, "POST", "/v1/shutdown", None).await.unwrap().0, 401);

    // agentd answers /v1/health with the documented shape and a live runner.
    let (status, body) = http(port, "GET", "/v1/health", Some(TOKEN)).await.unwrap();
    assert_eq!(status, 200);
    let health: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["ok"], true);
    assert!(health["version"].as_str().is_some_and(|v| !v.is_empty()));
    assert_eq!(health["agent"]["running"], true);

    // The stub's JSONL stdout arrives on the events socket stamped with seq and ts, and the initial
    // prompt was delivered to it as user_message id "initial".
    let mut events = ws(port, "/v1/events?since=0", Some(TOKEN)).await.unwrap();
    let opening = collect_until(&mut events, |e| e["type"] == "status" && e["state"] == "waiting_for_answer").await;
    assert_eq!(opening[0]["seq"], 1, "seq starts at 1");
    assert_stamped(&opening);
    assert!(
        has(&opening, |e| e["type"] == "status" && e["state"] == "idle"),
        "the runner's own stdout came through"
    );
    assert!(has(&opening, |e| e["type"] == "user_message"
        && e["id"] == "initial"
        && e["text"] == "boot check"));
    assert!(has(&opening, |e| {
        e["type"] == "question"
            && e["question_id"] == "q-smoke"
            && e["questions"][0]["question"] == "Ship it?"
            && e["questions"][0]["options"].as_array().is_some_and(|o| o.len() == 2)
    }));

    // The documented answer hop: the answer command reaches the runner's stdin and comes back as
    // question_answered with the same mapping, closing the turn.
    events
        .send(Message::Text(
            json!({"type": "answer", "question_id": "q-smoke", "answers": {"Ship it?": "Yes"}, "response": null})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let answered = collect_until(&mut events, |e| e["type"] == "status" && e["state"] == "idle").await;
    assert_stamped(&answered);
    assert!(
        has(&answered, |e| e["type"] == "question_answered"
            && e["question_id"] == "q-smoke"
            && e["answers"]["Ship it?"] == "Yes"),
        "the runner confirmed the answer it was sent: {answered:#?}"
    );
    let turn1 = answered
        .iter()
        .find(|e| e["type"] == "turn_end" && e["result"] == "answered")
        .unwrap()["seq"]
        .as_u64()
        .unwrap();

    // Health reflects what just happened: idle runner at the position the stream shows.
    let health: Value = serde_json::from_str(&http(port, "GET", "/v1/health", Some(TOKEN)).await.unwrap().1).unwrap();
    assert_eq!(health["agent"]["state"], "idle");
    assert_eq!(health["agent"]["running"], true);
    assert_eq!(health["agent"]["last_seq"], answered.last().unwrap()["seq"]);

    // A second client resuming from `since` replays exactly the stored events after it, then stays
    // live for the next turn.
    let mut resumed = ws(port, &format!("/v1/events?since={turn1}"), Some(TOKEN)).await.unwrap();
    let replayed_first = collect_until(&mut resumed, |e| e["type"] == "status").await;
    assert_eq!(
        replayed_first[0]["seq"].as_u64().unwrap(),
        turn1 + 1,
        "replay starts right after `since`"
    );

    // A message posted in reaches the runner's stdin. agentd has no POST /v1/message: the documented
    // surface is a user_message frame on the events socket, which is also how the harness sends the
    // browser's chat input. The runner's echo coming back proves both hops.
    events
        .send(Message::Text(
            json!({"type": "user_message", "id": "u-1", "text": "hello colony"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let second_turn = collect_until(&mut resumed, |e| {
        e["type"] == "assistant_text" && e["text"] == "echo: hello colony"
    })
    .await;
    assert_stamped(&second_turn);
    assert!(has(&second_turn, |e| e["type"] == "user_message"
        && e["id"] == "u-1"
        && e["text"] == "hello colony"));
    assert!(
        has(&second_turn, |e| e["type"] == "status" && e["state"] == "working"),
        "the live turn arrived on the resuming client, not just the replay: {second_turn:#?}"
    );

    // Clean shutdown, both layers: the runner shuts down gracefully over POST /v1/shutdown, and the
    // daemon itself exits 0 on SIGTERM, which is how a colony's VM stop reaches it.
    let (status, body) = http(port, "POST", "/v1/shutdown", Some(TOKEN)).await.unwrap();
    assert_eq!((status, body.as_str()), (200, r#"{"ok":true}"#));
    let exit = collect_until(&mut events, |e| {
        e["type"] == "status" && e["state"] == "exited" && e["detail"] == "exit code 0"
    })
    .await;
    let last_seq = exit.last().unwrap()["seq"].as_u64().unwrap();
    let health: Value = serde_json::from_str(&http(port, "GET", "/v1/health", Some(TOKEN)).await.unwrap().1).unwrap();
    assert_eq!(health["agent"]["running"], false);
    assert_eq!(health["agent"]["state"], "exited");
    assert_eq!(health["agent"]["last_seq"], last_seq);

    // Everything observed is also on disk, numbered 1..=N with nothing missing: that file is the
    // replay source a reconnecting harness depends on.
    let log: Vec<Value> = std::fs::read_to_string(dir.join("state/events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let seqs: Vec<u64> = log.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
    assert_eq!(
        seqs,
        (1..=last_seq).collect::<Vec<u64>>(),
        "the event log is complete and gap-free"
    );

    // The daemon's own shutdown: SIGTERM stops it cleanly with the runner already gone.
    unsafe { libc::kill(daemon.child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(daemon.child.wait().unwrap().code(), Some(0), "agentd exits 0 on SIGTERM");

    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}
