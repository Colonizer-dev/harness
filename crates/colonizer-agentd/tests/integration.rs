//! Runs the real colonizer-agentd binary against a fake agent runner (python3).

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

const TOKEN: &str = "test-token-123";

const FAKE_RUNNER: &str = r#"
import json, os, sys

def emit(**event):
    sys.stdout.write(json.dumps(event) + "\n")
    sys.stdout.flush()

sys.stderr.write("fake runner started\n")
sys.stderr.flush()
print("this is not an event", flush=True)
emit(type="status", state="idle", detail=os.environ.get("FAKE_GREETING", ""))
for line in sys.stdin:
    command = json.loads(line)
    kind = command.get("type")
    if kind == "user_message":
        emit(type="user_message", id=command["id"], text=command["text"])
        emit(type="status", state="working")
        emit(type="assistant_text", message_id="m-" + command["id"], block_index=0, text="echo: " + command["text"])
        emit(type="turn_end", is_error=False, result="echo: " + command["text"], cost_usd=None, duration_ms=1)
        emit(type="status", state="idle")
    elif kind == "shutdown":
        emit(type="status", state="exited", detail="shutdown requested")
        sys.exit(0)
    else:
        emit(type="log", level="info", message="got " + str(kind))
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
    let dir = std::env::temp_dir().join(format!(
        "colonizer-agentd-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("workspace")).unwrap();
    dir
}

async fn start(dir: &Path, initial_prompt: &str) -> Daemon {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(dir.join("runner.py"), FAKE_RUNNER).unwrap();
    std::fs::write(dir.join("token"), format!("{TOKEN}\n")).unwrap();
    let config = json!({
        "session_id": "test",
        "workspace": dir.join("workspace"),
        "listen": format!("127.0.0.1:{port}"),
        "agent": {"module": "fake", "command": ["python3", dir.join("runner.py")], "env": {"FAKE_GREETING": "hello-env"}},
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

async fn http(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

async fn ws(port: u16, path: &str, token: Option<&str>) -> Result<Ws, WsError> {
    let mut request = format!("ws://127.0.0.1:{port}{path}")
        .into_client_request()
        .unwrap();
    if let Some(token) = token {
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_auth_replay_commands_shutdown_and_restart() {
    let dir = scratch("events");
    let daemon = start(&dir, "hello").await;
    let port = daemon.port;

    // Auth on every endpoint.
    assert_eq!(http(port, "GET", "/v1/health", None).await.unwrap().0, 401);
    assert_eq!(
        http(port, "GET", "/v1/health", Some("wrong-token"))
            .await
            .unwrap()
            .0,
        401
    );
    assert_eq!(
        http(port, "POST", "/v1/shutdown", None).await.unwrap().0,
        401
    );
    match ws(port, "/v1/events?since=0", None).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!(
            "expected 401 for unauthenticated websocket, got {:?}",
            other.map(|_| ())
        ),
    }
    let (status, body) = http(port, "GET", "/v1/health", Some(TOKEN)).await.unwrap();
    assert_eq!(status, 200);
    let health: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(health["ok"], true);
    assert_eq!(health["agent"]["running"], true);

    // Initial prompt is delivered as user_message id "initial"; seq starts at 1 and increases.
    let mut events = ws(port, "/v1/events?since=0", Some(TOKEN)).await.unwrap();
    let first = collect_until(&mut events, |e| e["type"] == "turn_end").await;
    assert_eq!(first[0]["seq"], 1);
    for pair in first.windows(2) {
        assert_eq!(
            pair[1]["seq"].as_u64().unwrap(),
            pair[0]["seq"].as_u64().unwrap() + 1
        );
        assert!(pair[1]["ts"].is_string());
    }
    assert!(has(&first, |e| e["type"] == "user_message"
        && e["id"] == "initial"
        && e["text"] == "hello"));
    assert!(has(&first, |e| e["type"] == "assistant_text"
        && e["text"] == "echo: hello"));
    assert!(
        has(&first, |e| e["type"] == "status"
            && e["detail"] == "hello-env"),
        "agent.env is merged"
    );
    let turn1 = first.last().unwrap()["seq"].as_u64().unwrap();

    // Commands from a client reach the runner.
    events
        .send(Message::Text(
            json!({"type": "user_message", "id": "u-1", "text": "second"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let second = collect_until(&mut events, |e| {
        e["type"] == "turn_end" && e["result"] == "echo: second"
    })
    .await;
    assert!(has(&second, |e| e["type"] == "assistant_text"
        && e["text"] == "echo: second"));
    events
        .send(Message::Text(
            json!({"type": "interrupt"}).to_string().into(),
        ))
        .await
        .unwrap();
    events
        .send(Message::Text(
            json!({"type": "shutdown"}).to_string().into(),
        ))
        .await
        .unwrap(); // not forwarded
    collect_until(&mut events, |e| {
        e["type"] == "log" && e["message"] == "got interrupt"
    })
    .await;

    // Replay from a later point starts exactly after `since`.
    let mut replay = ws(port, &format!("/v1/events?since={turn1}"), Some(TOKEN))
        .await
        .unwrap();
    let replayed = collect_until(&mut replay, |e| {
        e["type"] == "log" && e["message"] == "got interrupt"
    })
    .await;
    assert_eq!(replayed[0]["seq"].as_u64().unwrap(), turn1 + 1);
    assert!(has(&replayed, |e| e["type"] == "assistant_text"
        && e["text"] == "echo: second"));

    // Graceful shutdown.
    let (status, body) = http(port, "POST", "/v1/shutdown", Some(TOKEN))
        .await
        .unwrap();
    assert_eq!((status, body.as_str()), (200, r#"{"ok":true}"#));
    let exit = collect_until(&mut events, |e| {
        e["type"] == "status"
            && e["state"] == "exited"
            && e["detail"]
                .as_str()
                .is_some_and(|d| d.starts_with("exit code"))
    })
    .await;
    let last_seq = exit.last().unwrap()["seq"].as_u64().unwrap();
    let health: Value = serde_json::from_str(
        &http(port, "GET", "/v1/health", Some(TOKEN))
            .await
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(health["agent"]["running"], false);
    assert_eq!(health["agent"]["state"], "exited");

    // stderr and non-JSON stdout become warn logs; everything is in the log file.
    let log: Vec<Value> = std::fs::read_to_string(dir.join("state/events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(has(&log, |e| e["type"] == "log"
        && e["level"] == "warn"
        && e["message"] == "fake runner started"));
    assert!(has(&log, |e| e["level"] == "warn"
        && e["message"]
            .as_str()
            .unwrap_or("")
            .contains("non-event line")));
    assert!(
        !has(&log, |e| e["type"] == "log"
            && e["message"] == "got shutdown"),
        "shutdown from WS is not forwarded"
    );

    // A restarted agentd continues numbering from the existing log.
    drop(events);
    drop(replay);
    drop(daemon);
    let daemon = start(&dir, "").await;
    let mut events = ws(
        daemon.port,
        &format!("/v1/events?since={last_seq}"),
        Some(TOKEN),
    )
    .await
    .unwrap();
    let next = collect_until(&mut events, |_| true).await;
    assert_eq!(next[0]["seq"].as_u64().unwrap(), last_seq + 1);

    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pty_roundtrip_with_resize_and_exit() {
    let dir = scratch("pty");
    let daemon = start(&dir, "").await;
    let mut pty = ws(daemon.port, "/v1/pty?cols=100&rows=30", Some(TOKEN))
        .await
        .unwrap();

    pty.send(Message::Text(
        json!({"type": "resize", "cols": 120, "rows": 40})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    pty.send(Message::Binary(
        b"stty size; echo hi-$((40+2)); pwd\n".to_vec().into(),
    ))
    .await
    .unwrap();

    let mut output = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !(output.contains("hi-42") && output.contains("40 120")) {
        match tokio::time::timeout_at(deadline, pty.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                output.push_str(&String::from_utf8_lossy(&bytes))
            }
            Ok(Some(Ok(_))) => {}
            other => panic!("terminal output ended early ({other:?}); got: {output}"),
        }
    }
    assert!(
        output.contains("workspace"),
        "shell starts in the workspace: {output}"
    );

    pty.send(Message::Binary(b"exit 3\n".to_vec().into()))
        .await
        .unwrap();
    let exit = loop {
        match tokio::time::timeout_at(deadline, pty.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                break serde_json::from_str::<Value>(&text).unwrap();
            }
            Ok(Some(Ok(_))) => {}
            other => panic!("no exit frame: {other:?}"),
        }
    };
    assert_eq!(exit, json!({"type": "exit", "code": 3}));

    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}
