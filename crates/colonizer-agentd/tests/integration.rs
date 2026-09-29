//! Runs the real colonizer-agentd binary against a fake agent runner (python3).

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
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
    let dir = std::env::temp_dir().join(format!("colonizer-agentd-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(dir.join("workspace")).unwrap();
    dir
}

async fn start(dir: &Path, initial_prompt: &str) -> Daemon {
    let (child, port) = spawn_daemon(dir, initial_prompt, TOKEN, &[]);
    wait_healthy(child, port).await
}

/// Writes the fixture (runner, token, session.json) and spawns the real agentd binary on a free
/// port, with any extra command-line arguments (`--seal-token` for the seal tests below).
fn spawn_daemon(dir: &Path, initial_prompt: &str, token: &str, extra_args: &[&str]) -> (Child, u16) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(dir.join("runner.py"), FAKE_RUNNER).unwrap();
    std::fs::write(dir.join("token"), format!("{token}\n")).unwrap();
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
        .args(extra_args)
        .env("HOME", dir)
        // Piped, so a refusal test can read the startup error the way a boot log would.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    (child, port)
}

async fn wait_healthy(child: Child, port: u16) -> Daemon {
    let daemon = Daemon { child, port };
    for _ in 0..100 {
        if let Ok((200, _)) = http(port, "GET", "/v1/health", Some(TOKEN)).await {
            return daemon;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("colonizer-agentd did not become healthy");
}

/// Waits for a short-lived agentd that is supposed to refuse to start. A daemon that stays up is
/// killed and fails the test loudly instead of the test hanging on `wait_with_output` forever.
fn wait_exit(mut child: Child, seconds: u64) -> std::process::Output {
    let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("colonizer-agentd neither refused nor exited within {seconds}s (it started and kept running)");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_auth_replay_commands_shutdown_and_restart() {
    let dir = scratch("events");
    let daemon = start(&dir, "hello").await;
    let port = daemon.port;

    // Auth on every endpoint.
    assert_eq!(http(port, "GET", "/v1/health", None).await.unwrap().0, 401);
    assert_eq!(http(port, "GET", "/v1/health", Some("wrong-token")).await.unwrap().0, 401);
    assert_eq!(http(port, "POST", "/v1/shutdown", None).await.unwrap().0, 401);
    match ws(port, "/v1/events?since=0", None).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected 401 for unauthenticated websocket, got {:?}", other.map(|_| ())),
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
        assert_eq!(pair[1]["seq"].as_u64().unwrap(), pair[0]["seq"].as_u64().unwrap() + 1);
        assert!(pair[1]["ts"].is_string());
    }
    assert!(has(&first, |e| e["type"] == "user_message"
        && e["id"] == "initial"
        && e["text"] == "hello"));
    assert!(has(&first, |e| e["type"] == "assistant_text" && e["text"] == "echo: hello"));
    assert!(
        has(&first, |e| e["type"] == "status" && e["detail"] == "hello-env"),
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
    let second = collect_until(&mut events, |e| e["type"] == "turn_end" && e["result"] == "echo: second").await;
    assert!(has(&second, |e| e["type"] == "assistant_text" && e["text"] == "echo: second"));
    events
        .send(Message::Text(json!({"type": "interrupt"}).to_string().into()))
        .await
        .unwrap();
    events
        .send(Message::Text(
            json!({"type": "set_model", "model": "sonnet"}).to_string().into(),
        ))
        .await
        .unwrap();
    events
        .send(Message::Text(json!({"type": "shutdown"}).to_string().into()))
        .await
        .unwrap(); // not forwarded
    let forwarded = collect_until(&mut events, |e| e["type"] == "log" && e["message"] == "got set_model").await;
    assert!(has(&forwarded, |e| e["type"] == "log" && e["message"] == "got interrupt"));

    // Replay from a later point starts exactly after `since`.
    let mut replay = ws(port, &format!("/v1/events?since={turn1}"), Some(TOKEN)).await.unwrap();
    let replayed = collect_until(&mut replay, |e| e["type"] == "log" && e["message"] == "got interrupt").await;
    assert_eq!(replayed[0]["seq"].as_u64().unwrap(), turn1 + 1);
    assert!(has(&replayed, |e| e["type"] == "assistant_text" && e["text"] == "echo: second"));

    // Graceful shutdown.
    let (status, body) = http(port, "POST", "/v1/shutdown", Some(TOKEN)).await.unwrap();
    assert_eq!((status, body.as_str()), (200, r#"{"ok":true}"#));
    let exit = collect_until(&mut events, |e| {
        e["type"] == "status" && e["state"] == "exited" && e["detail"].as_str().is_some_and(|d| d.starts_with("exit code"))
    })
    .await;
    let last_seq = exit.last().unwrap()["seq"].as_u64().unwrap();
    let health: Value = serde_json::from_str(&http(port, "GET", "/v1/health", Some(TOKEN)).await.unwrap().1).unwrap();
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
        && e["message"].as_str().unwrap_or("").contains("non-event line")));
    assert!(
        !has(&log, |e| e["type"] == "log" && e["message"] == "got shutdown"),
        "shutdown from WS is not forwarded"
    );

    // A restarted agentd continues numbering from the existing log.
    drop(events);
    drop(replay);
    drop(daemon);
    let daemon = start(&dir, "").await;
    let mut events = ws(daemon.port, &format!("/v1/events?since={last_seq}"), Some(TOKEN))
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
    let mut pty = ws(daemon.port, "/v1/pty?cols=100&rows=30", Some(TOKEN)).await.unwrap();

    pty.send(Message::Text(
        json!({"type": "resize", "cols": 120, "rows": 40}).to_string().into(),
    ))
    .await
    .unwrap();
    pty.send(Message::Binary(b"stty size; echo hi-$((40+2)); pwd\n".to_vec().into()))
        .await
        .unwrap();

    let mut output = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !(output.contains("hi-42") && output.contains("40 120")) {
        match tokio::time::timeout_at(deadline, pty.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => output.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(Some(Ok(_))) => {}
            other => panic!("terminal output ended early ({other:?}); got: {output}"),
        }
    }
    assert!(output.contains("workspace"), "shell starts in the workspace: {output}");

    pty.send(Message::Binary(b"exit 3\n".to_vec().into())).await.unwrap();
    let exit = loop {
        match tokio::time::timeout_at(deadline, pty.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => break serde_json::from_str::<Value>(&text).unwrap(),
            Ok(Some(Ok(_))) => {}
            other => panic!("no exit frame: {other:?}"),
        }
    };
    assert_eq!(exit, json!({"type": "exit", "code": 3}));

    drop(daemon);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Whether this environment may apply the bind mount the seal needs (Linux root with the mount
/// capabilities, e.g. CAP_SYS_ADMIN): a throwaway bind of /dev/null over a scratch file, undone
/// right after. Off Linux this is simply false — the truth, since the seal would refuse there.
#[cfg(target_os = "linux")]
fn mount_permitted(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let probe = dir.join("seal-probe");
    std::fs::write(&probe, b"x").unwrap();
    let target = std::ffi::CString::new(probe.as_os_str().as_bytes()).unwrap();
    let source = b"/dev/null\0";
    // SAFETY: both arguments are NUL-terminated C strings.
    let ok = unsafe {
        libc::mount(
            source.as_ptr().cast(),
            target.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    } == 0;
    if ok {
        unsafe { libc::umount(target.as_ptr()) };
    }
    let _ = std::fs::remove_file(&probe);
    ok
}

#[cfg(not(target_os = "linux"))]
fn mount_permitted(_dir: &Path) -> bool {
    false
}

/// Undoes a seal's bind mount, so the scratch dir can be removed afterwards.
#[cfg(target_os = "linux")]
fn unmount_seal(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let target = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the argument is a NUL-terminated C string.
    unsafe { libc::umount(target.as_ptr()) };
}

#[cfg(not(target_os = "linux"))]
fn unmount_seal(_path: &Path) {}

/// Unmounts everything mounted anywhere under `dir`, deepest first — every bind the path-policy
/// watcher applied — so the scratch tree can be removed afterwards.
#[cfg(target_os = "linux")]
fn unmount_tree(dir: &Path) {
    use std::os::unix::ffi::OsStringExt;
    let Ok(bytes) = std::fs::read("/proc/self/mountinfo") else {
        return;
    };
    let mut points: Vec<PathBuf> = bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| line.split(|b| *b == b' ').nth(4))
        .map(|field| PathBuf::from(std::ffi::OsString::from_vec(field.to_vec())))
        .filter(|p| p.starts_with(dir))
        .collect();
    points.sort_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
    for point in points {
        if let Ok(c) = std::ffi::CString::new(point.as_os_str().as_encoded_bytes()) {
            // SAFETY: the argument is a NUL-terminated C string.
            unsafe { libc::umount(c.as_ptr()) };
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn unmount_tree(_dir: &Path) {}

/// Waits until the event stream carries a log message containing each needle, and answers
/// everything seen on the way.
async fn wait_for_logs(port: u16, needles: &[&str]) -> Vec<Value> {
    let mut events = ws(port, "/v1/events?since=0", Some(TOKEN)).await.unwrap();
    let mut seen = Vec::new();
    let missing = |seen: &[Value]| {
        needles.iter().any(|n| {
            !seen
                .iter()
                .any(|e| e["type"] == "log" && e["message"].as_str().is_some_and(|m| m.contains(n)))
        })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while missing(&seen) {
        let message = tokio::time::timeout_at(deadline, events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {needles:?}; events so far: {seen:#?}"))
            .expect("event stream ended")
            .expect("websocket error");
        if let Message::Text(text) = message {
            seen.push(serde_json::from_str(&text).unwrap());
        }
    }
    drop(events);
    seen
}

/// An empty (after trim) token file stops the start (`main.rs` refuses): a `Bearer ` header with
/// nothing after it must never authorize anything, and a sealed token path (#640) reads exactly
/// empty, so a re-read from disk must never become the daemon's token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_token_file_refuses_to_start() {
    let dir = scratch("empty-token");
    let (child, _port) = spawn_daemon(&dir, "", "   \n", &[]);
    let output = wait_exit(child, 10);
    assert!(!output.status.success(), "an empty token file must refuse to start");
    let say = String::from_utf8_lossy(&output.stderr);
    assert!(say.contains("is empty"), "the error says what is wrong: {say}");
    assert!(
        say.contains(dir.join("token").to_str().unwrap()),
        "the error names the path: {say}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A seal that cannot be applied stops the start and names the path (fail closed, #640). On hosts
/// where mounts are unavailable — stock CI runs unprivileged, macOS has no seal at all — this is
/// the live branch; where they work, the privileged test below covers it and this one steps aside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_seal_refuses_to_start() {
    let dir = scratch("seal-refusal");
    if mount_permitted(&dir) {
        eprintln!("skipping: this host permits mounts, so the seal succeeds; the privileged seal test covers the happy path");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let (child, _port) = spawn_daemon(&dir, "", TOKEN, &["--seal-token"]);
    let output = wait_exit(child, 10);
    assert!(!output.status.success(), "a failed seal must refuse to start");
    let say = String::from_utf8_lossy(&output.stderr);
    assert!(
        say.contains("cannot seal"),
        "the refusal must be the seal itself, not a silent start or an unknown flag: {say}"
    );
    assert!(
        say.contains(dir.join("token").to_str().unwrap()),
        "the error names the path: {say}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The seal end to end, on a host that can apply it (skips elsewhere with a note, like harden.rs's
/// probe): with `--seal-token` agentd reads the token and covers the path, and from then on the
/// runner's world sees an empty file. A hardened child (`--exec-hardened` is the exact runner
/// profile) reads nothing, the `Bearer` it could send opens no `/v1/pty` (401), and it cannot
/// `umount` the seal back; the daemon itself keeps serving the real token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seal_token_hides_the_token_from_the_runner_and_the_pty() {
    let dir = scratch("seal");
    if !mount_permitted(&dir) {
        eprintln!(
            "skipping: the seal is a bind mount and this environment (euid {}, mount denied) cannot apply one; the fail-closed test covers the refusal branch",
            unsafe { libc::geteuid() }
        );
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let token_path = dir.join("token");
    let (child, port) = spawn_daemon(&dir, "", TOKEN, &["--seal-token"]);
    // Undoes the mount and the dir even when an assertion fires mid-test: a leaked bind would keep
    // the scratch dir unremovable.
    struct Sealed(PathBuf);
    impl Drop for Sealed {
        fn drop(&mut self) {
            unmount_seal(&self.0.join("token"));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Sealed(dir.clone());
    let daemon = wait_healthy(child, port).await;

    // Every later reader in the namespace — the runner's read included — sees an empty file.
    assert_eq!(std::fs::read(&token_path).unwrap(), b"" as &[u8], "the path reads empty");
    let read = Command::new(env!("CARGO_BIN_EXE_colonizer-agentd"))
        .arg("--exec-hardened")
        .arg("--")
        .arg("cat")
        .arg(&token_path)
        .output()
        .unwrap();
    assert!(read.status.success(), "the hardened read itself works");
    assert!(
        read.stdout.is_empty(),
        "the runner profile reads an empty token, got {:?}",
        String::from_utf8_lossy(&read.stdout)
    );

    // The bearer a sealed read yields opens nothing; the daemon's own copy still does.
    match ws(port, "/v1/pty", Some("")).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!(
            "expected 401 for the empty bearer a sealed read yields, got {:?}",
            other.map(|_| ())
        ),
    }
    let (status, _) = http(port, "GET", "/v1/health", Some(TOKEN)).await.unwrap();
    assert_eq!(status, 200);
    ws(port, "/v1/pty", Some(TOKEN))
        .await
        .expect("the real token still opens the terminal");

    // And the hardened child cannot tear the seal down to read the token after all.
    let umount = Command::new(env!("CARGO_BIN_EXE_colonizer-agentd"))
        .arg("--exec-hardened")
        .arg("--")
        .arg("umount")
        .arg(&token_path)
        .output()
        .unwrap();
    assert!(!umount.status.success(), "the runner profile cannot umount the seal");

    drop(daemon);
}

/// The path policy beyond boot (issue #648): a nested checkout created mid-session — staged
/// whole next to the workspace and renamed in, the way a clone or a `git init` lands — gets the
/// policy's binds as its paths appear, while a `.env` at the workspace root (no nested checkout;
/// the boot's business) is left alone. The protected `.git/config` the boot never binds — the
/// root's git dir is host-mounted read-only instead — is covered for a nested checkout too. Where
/// mount(2) is unavailable (stock CI), the same run asserts the watcher's fail-soft branch
/// instead: it finds the checkout and names every bind it could not apply, warn events, never a
/// stopped daemon.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_path_policy_covers_a_nested_checkout_created_mid_session() {
    let dir = scratch("path-policy");
    let mounted = mount_permitted(&dir);
    let policy = dir.join("path-policy");
    std::fs::write(&policy, "mask-file .env\nprotect AGENTS.md\n").unwrap();
    let (child, port) = spawn_daemon(&dir, "", TOKEN, &[&format!("--path-policy={}", policy.display())]);
    // Undoes every bind below the scratch dir even when an assertion fires mid-test: a leaked
    // mount would keep the tree unremovable.
    struct Mounted(PathBuf);
    impl Drop for Mounted {
        fn drop(&mut self) {
            unmount_tree(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Mounted(dir.clone());
    let daemon = wait_healthy(child, port).await;

    // The checkout appears after boot, whole: written first in a sibling the watcher has no
    // interest in, then renamed into place, so no `.git`-half-written state can race the binds.
    let staged = dir.join("staged");
    std::fs::create_dir_all(staged.join(".git")).unwrap();
    std::fs::write(staged.join(".git/config"), "[core]").unwrap();
    std::fs::write(staged.join(".env"), "SECRET=1").unwrap();
    std::fs::write(staged.join("AGENTS.md"), "rules").unwrap();
    std::fs::create_dir(dir.join("workspace/vendor")).unwrap();
    std::fs::rename(&staged, dir.join("workspace/vendor/lib")).unwrap();
    let checkout = dir.join("workspace/vendor/lib");
    std::fs::write(dir.join("workspace/.env"), "ROOT=1").unwrap();

    let applied = [
        "masked `vendor/lib/.env`",
        "protected `vendor/lib/AGENTS.md`",
        "protected `vendor/lib/.git/config`",
    ];
    let needles: Vec<String> = applied
        .iter()
        .map(|bind| {
            if mounted {
                format!("{bind} (nested checkout `vendor/lib`)")
            } else {
                format!("cannot apply {bind}:")
            }
        })
        .collect();
    let refs: Vec<&str> = needles.iter().map(String::as_str).collect();
    let seen = wait_for_logs(port, &refs).await;
    // The needle a wrongly-bound root `.env` would actually produce — the events spell paths
    // workspace-relative, so the root's own is bare.
    let stray = if mounted {
        "masked `.env`"
    } else {
        "cannot apply masked `.env`:"
    };
    assert!(
        !has(&seen, |e| e["message"].as_str().is_some_and(|m| m.contains(stray))),
        "the workspace root is not the watcher's business: {seen:#?}"
    );

    // With the binds live: the nested .env reads empty, the protected paths refuse writes.
    if mounted {
        assert_eq!(
            std::fs::read(checkout.join(".env")).unwrap(),
            b"" as &[u8],
            "the nested .env reads empty"
        );
        assert_eq!(
            std::fs::read(dir.join("workspace/.env")).unwrap(),
            b"ROOT=1",
            "the workspace-root .env is untouched"
        );
        for protected in ["AGENTS.md", ".git/config"] {
            let write = std::fs::write(checkout.join(protected), "rewritten");
            assert!(write.is_err(), "the nested {protected} is read-only");
        }
    }

    drop(daemon);
}
