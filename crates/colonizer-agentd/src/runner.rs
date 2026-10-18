//! The agent runner module process (docs/protocol.md §2): commands on stdin, events on stdout,
//! stderr lines become `log` events.

use crate::{
    config::SessionConfig,
    harden,
    store::{EventStore, log_event, status_event},
};
use serde_json::{Value, json};
use std::{
    os::unix::process::ExitStatusExt,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, Command},
    sync::{mpsc, oneshot, watch},
};

/// The status-event detail for a spawn failure: it names the binary, so the surfaced
/// error/attention says what failed to start instead of reading as a bare harness complaint.
/// (The log line already names it; this keeps the status detail — the one events.rs matches on —
/// just as explicit.)
fn spawn_failure_detail(program: &str, err: &std::io::Error) -> String {
    format!("cannot start agent runner `{program}`: {err}")
}

const MAX_INVALID_LINE: usize = 500;

/// The longest runner line read whole, in bytes. Runner output is untrusted in size, so
/// `for_each_line` never buffers a line past this — the bound colonizer-observability puts on a
/// line (cursor.rs `Limits::max_line_bytes`), taken at the mothership's own cap on a colony log
/// line (`history::MAX_LINE_BYTES`), which a 2 MiB runner line must also fall under.
const MAX_LINE_BYTES: usize = 256 * 1024;

/// How much of a runner stream `for_each_line` reads at once (cursor.rs `CHUNK`).
const CHUNK: usize = 64 * 1024;

pub struct Runner {
    commands: mpsc::UnboundedSender<String>,
    running: watch::Receiver<bool>,
    /// Flipped by [`Runner::shutdown`] before or after launch: it makes a launch that has not
    /// happened yet a no-op, and wakes the daemon's setup-hook race.
    stopped: watch::Sender<bool>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
    /// The other ends of the channels above, held until [`Runner::launch`] hands them to the child
    /// supervisor. `None` once launched.
    launch: Mutex<Option<Launch>>,
}

/// The receiving ends of a [`Runner`]'s channels — what the child supervisor needs — kept aside
/// until the brief is final and [`Runner::launch`] starts the child.
struct Launch {
    commands: mpsc::UnboundedReceiver<String>,
    running: watch::Sender<bool>,
    kill: oneshot::Receiver<()>,
}

impl Runner {
    pub fn is_running(&self) -> bool {
        *self.running.borrow()
    }

    /// Queues a command for the runner's stdin; false when no runner can still receive it. Before
    /// launch it queues until the child starts, so commands that arrive during the setup hook are
    /// delivered rather than dropped; after the child exits (or a shutdown) it is refused.
    pub fn send(&self, command: &Value) -> bool {
        self.takes_commands() && self.commands.send(command.to_string()).is_ok()
    }

    fn takes_commands(&self) -> bool {
        !self.is_stopped() && (self.is_running() || self.launch.lock().unwrap().is_some())
    }

    /// Whether a shutdown has been requested, before or after launch. A launch after this is a
    /// no-op, so a shutdown during the setup hook cannot still start the agent.
    pub fn is_stopped(&self) -> bool {
        *self.stopped.borrow()
    }

    /// Resolves once [`Runner::shutdown`] is called; the daemon races it against the setup hook, so
    /// a shutdown mid-hook kills the hook instead of orphaning it.
    pub async fn stopped(&self) {
        let _ = self.stopped.subscribe().wait_for(|stopped| *stopped).await;
    }

    /// Asks the runner to exit, then kills it if it hasn't within 10 s. Before launch there is no
    /// child to ask, and this only records the stop (see [`Runner::stopped`]).
    pub async fn shutdown(&self) {
        self.stopped.send_replace(true);
        if !self.is_running() {
            return;
        }
        let _ = self.commands.send(json!({"type": "shutdown"}).to_string());
        if !self.wait_exit(Duration::from_secs(10)).await {
            if let Some(kill) = self.kill.lock().unwrap().take() {
                let _ = kill.send(());
            }
            self.wait_exit(Duration::from_secs(5)).await;
        }
    }

    async fn wait_exit(&self, limit: Duration) -> bool {
        let mut running = self.running.clone();
        tokio::time::timeout(limit, async move { running.wait_for(|r| !*r).await.is_ok() })
            .await
            .unwrap_or(false)
    }
}

impl Runner {
    /// Starts the agent child on the final brief. Split from [`create`] so the daemon can serve
    /// /v1/health and /v1/events while the repository's setup hook runs (issue #753): a long hook is
    /// then a delay to the agent, not to the mothership's boot wait. A second call does nothing.
    pub fn launch(&self, config: &SessionConfig, store: Arc<EventStore>) {
        if self.is_stopped() {
            return;
        }
        let Some(Launch { commands, running, kill }) = self.launch.lock().unwrap().take() else {
            return;
        };
        let Some((program, args)) = config.agent.command.split_first() else {
            store.append(status_event("error", Some("agent.command is empty".into())));
            return;
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&config.workspace)
            .envs(&config.agent.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // The runner child runs the agent's untrusted code, so it is spawned hardened (harden.rs): the
        // capability bounding set trimmed, no core dumps, no_new_privs, and a seccomp denylist whose
        // denials surface as ordinary EPERM tool failures. Fail closed: a failed hardening fails the
        // spawn, through the same error path as any other spawn failure below.
        let hardening = harden::Hardening::prepare();
        store.append(log_event("info", format!("hardening: {}", hardening.describe())));
        unsafe { command.pre_exec(hardening.guard()) };
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                store.append(log_event("error", format!("cannot start agent runner `{program}`: {e}")));
                store.append(status_event("error", Some(spawn_failure_detail(program, &e))));
                return;
            }
        };
        running.send_replace(true);
        // Queue the brief before `feed_stdin` starts draining, so it is delivered ahead of any
        // command that arrived during the setup hook and was buffered in the same channel.
        if let Some(prompt) = config.initial_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            self.send(&json!({"type": "user_message", "id": "initial", "text": prompt}));
        }

        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            unreachable!("runner stdio is piped");
        };
        tokio::spawn(feed_stdin(stdin, commands));
        let stdout_task = tokio::spawn({
            let store = store.clone();
            for_each_line(stdout, move |line| match serde_json::from_str::<Value>(line) {
                Ok(Value::Object(event)) if event.get("type").is_some_and(Value::is_string) => {
                    store.append(event);
                }
                _ => {
                    store.append(log_event(
                        "warn",
                        format!("agent runner wrote a non-event line: {}", truncate(line, MAX_INVALID_LINE)),
                    ));
                }
            })
        });
        let stderr_task = tokio::spawn({
            let store = store.clone();
            // stderr is runner diagnostics, not payload: it goes through the same bounded
            // excerpt as a non-event stdout line, so no path off the runner's pipes stores an
            // unbounded line.
            for_each_line(stderr, move |line| {
                store.append(log_event("warn", truncate(line, MAX_INVALID_LINE)));
            })
        });

        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                Ok(()) = kill => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            // Let the runner's last events land before announcing the exit.
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                let _ = stdout_task.await;
                let _ = stderr_task.await;
            })
            .await;
            running.send_replace(false);
            let detail = match status {
                Ok(status) => match (status.code(), status.signal()) {
                    (Some(code), _) => format!("exit code {code}"),
                    (None, Some(signal)) => format!("killed by signal {signal}"),
                    _ => "exited".into(),
                },
                Err(e) => format!("wait failed: {e}"),
            };
            store.append(status_event("exited", Some(detail)));
        });
    }
}

/// A runner whose child has not started yet, so the HTTP server can serve /v1/health and /v1/events
/// (the mothership's boot wait) while the repository's setup hook runs. [`Runner::launch`] starts it.
pub fn create() -> Arc<Runner> {
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (running_tx, running) = watch::channel(false);
    let (stopped, _) = watch::channel(false);
    let (kill_tx, kill_rx) = oneshot::channel();
    Arc::new(Runner {
        commands,
        running,
        stopped,
        kill: Mutex::new(Some(kill_tx)),
        launch: Mutex::new(Some(Launch {
            commands: command_rx,
            running: running_tx,
            kill: kill_rx,
        })),
    })
}

async fn feed_stdin(mut stdin: ChildStdin, mut commands: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = commands.recv().await {
        let written = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        };
        if written.await.is_err() {
            break;
        }
    }
}

/// Calls `f` for every non-empty line; invalid UTF-8 is replaced rather than ending the stream.
///
/// A line that runs past [`MAX_LINE_BYTES`] is never buffered whole, the way cursor.rs never
/// holds a line past its cap: its remainder is consumed in bounded chunks — counting the whole
/// line's bytes, and keeping whatever followed its newline so the stream stays aligned — and `f`
/// receives the line's head already truncated to [`MAX_INVALID_LINE`] chars with a marker naming
/// its full size. Every path that stores runner output (an event line, a non-event log, a stderr
/// log) then lands the same bounded text. A JSON event cut this way simply fails to parse and is
/// handled as a non-event line — acceptable for a pathological line, and the only cost of the cap.
async fn for_each_line(reader: impl AsyncRead + Unpin, mut f: impl FnMut(&str)) {
    let mut reader = reader;
    let mut chunk = vec![0u8; CHUNK];
    let mut buf: Vec<u8> = Vec::new(); // the line in hand, never past MAX_LINE_BYTES + 1
    loop {
        if let Some(i) = buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=i).collect();
            line.pop(); // the newline itself
            emit(&line, &mut f);
            continue;
        }
        // No newline in hand and the line is over the cap: consume its remainder a chunk at a
        // time — never holding it — so the next line still starts exactly after its newline.
        if buf.len() > MAX_LINE_BYTES {
            let mut total = buf.len() as u64; // the whole line's byte count, head included
            let head = String::from_utf8_lossy(&buf).into_owned();
            buf.clear();
            let mut ended = false; // EOF or a read error: this fragment is the stream's last line
            loop {
                match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => {
                        ended = true;
                        break;
                    }
                    Ok(n) => match chunk[..n].iter().position(|&b| b == b'\n') {
                        Some(i) => {
                            total += i as u64;
                            buf.extend_from_slice(&chunk[i + 1..n]); // the next line's start
                            break;
                        }
                        None => total += n as u64,
                    },
                }
            }
            f(&truncated_line(&head, total));
            if ended {
                break;
            }
            continue;
        }
        // Read more of the line in hand, bounded so `buf` can only just cross the cap: the
        // check above needs the byte past it to tell an over-cap line from an at-cap one.
        let want = CHUNK.min(MAX_LINE_BYTES + 1 - buf.len());
        match reader.read(&mut chunk[..want]).await {
            Ok(0) => {
                emit(&buf, &mut f); // a trailing fragment is the stream's last line
                break;
            }
            Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Hands one line to `f`: invalid UTF-8 is replaced, a whitespace-only line is skipped.
fn emit(line: &[u8], f: &mut impl FnMut(&str)) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if !line.is_empty() {
        f(line);
    }
}

/// The stored form of a line that ran past [`MAX_LINE_BYTES`]: its head, cut to leave the whole
/// string within [`MAX_INVALID_LINE`] chars, plus a marker naming how many bytes the full line
/// ran (cursor.rs marks the same event `GapReason::OversizedLine`). Kept within the cap so the
/// 500-char excerpt the callers apply leaves the marker intact.
fn truncated_line(head: &str, total_bytes: u64) -> String {
    let marker = format!("… [truncated {total_bytes} bytes]");
    let keep = MAX_INVALID_LINE - marker.chars().count();
    head.chars().take(keep).collect::<String>() + &marker
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_shutdown_before_launch_buffers_commands_then_stops_the_runner() {
        let runner = create();
        // Before launch a command is accepted and buffered (delivered once the child starts,
        // runner.rs `takes_commands`) rather than dropped as it was when send gated on `running`.
        assert!(runner.send(&json!({"type": "user_message"})));
        assert!(!runner.is_stopped());
        runner.shutdown().await;
        assert!(runner.is_stopped());
        // The daemon's setup-hook race awaits exactly this.
        runner.stopped().await;
        // Once stopped, nothing more is accepted, so a shutdown mid-hook cannot start the agent.
        assert!(!runner.send(&json!({"type": "user_message"})));
    }

    #[test]
    fn spawn_failure_detail_names_the_binary() {
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "No such file or directory");
        let detail = spawn_failure_detail("node", &err);
        assert!(
            detail.contains("cannot start agent runner"),
            "marker events.rs matches on: {detail}"
        );
        assert!(detail.contains("`node`"), "the binary must be named: {detail}");
        assert!(
            detail.contains("No such file or directory"),
            "the cause must survive: {detail}"
        );
    }

    /// `for_each_line`'s boundary arithmetic, driven directly over byte slices (it takes any
    /// `AsyncRead`, and a slice read returns exactly what is asked, so the chunking is
    /// deterministic): the cap is inclusive — a line of exactly `MAX_LINE_BYTES` lands whole,
    /// one byte more is truncated with its whole size in the marker — the skip keeps the stream
    /// aligned when the oversized line's newline shares a chunk with the start of the next line,
    /// and an oversized line cut short by EOF is emitted exactly once. `huge_runner_lines_are_
    /// capped` proves the cap end-to-end through a real runner at 2 MiB; this pins the
    /// off-by-ones (`>` vs `>=`) and the same-chunk stash that a 2 MiB line cannot see.
    #[tokio::test]
    async fn for_each_line_pins_the_cap_boundary_and_chunk_alignment() {
        async fn run(input: &[u8]) -> Vec<String> {
            let mut lines = Vec::new();
            for_each_line(input, |line| lines.push(line.to_string())).await;
            lines
        }

        /// The stdout closure's own test, so "still an event" means what `launch` means.
        fn is_event(line: &str) -> bool {
            matches!(
                serde_json::from_str::<Value>(line),
                Ok(Value::Object(event)) if event.get("type").is_some_and(Value::is_string)
            )
        }

        fn marker(total: usize) -> String {
            format!("[truncated {total} bytes]")
        }

        let after = r#"{"type":"log","message":"after the giant"}"#;
        // One case: its name, the bytes fed to `for_each_line`, and the assertions on the
        // lines `f` saw (named, so a failure says which case).
        type Case = (&'static str, Vec<u8>, Box<dyn Fn(&str, &[String])>);
        let cases: Vec<Case> = vec![
            (
                "an at-cap line lands whole",
                [vec![b'x'; MAX_LINE_BYTES], b"\ntail line\n".to_vec()].concat(),
                Box::new(|name, lines| {
                    assert!(lines.len() == 2, "{name}: at-cap line + the line after it, got {}", lines.len());
                    assert!(
                        lines[0].chars().count() == MAX_LINE_BYTES
                            && lines[0].starts_with("xxxx")
                            && !lines[0].contains("truncated"),
                        "{name}: a line of exactly MAX_LINE_BYTES is not oversized"
                    );
                    assert_eq!(lines[1], "tail line", "{name}");
                }),
            ),
            (
                "one byte over the cap is truncated, and the next line survives the skip",
                [vec![b'x'; MAX_LINE_BYTES + 1], format!("\n{after}\n").into_bytes()].concat(),
                Box::new(move |name, lines| {
                    assert!(lines.len() == 2, "{name}: over-cap line + the line after it, got {}", lines.len());
                    assert!(
                        lines[0].starts_with("xxxx")
                            && lines[0].ends_with(&marker(MAX_LINE_BYTES + 1))
                            && lines[0].chars().count() <= MAX_INVALID_LINE,
                        "{name}: the over-cap line is a bounded excerpt carrying its whole size"
                    );
                    assert_eq!(lines[1], after, "{name}: the line after the skip must arrive intact");
                }),
            ),
            (
                "EOF mid-oversized-line emits it exactly once",
                vec![b'x'; MAX_LINE_BYTES + 1000],
                Box::new(|name, lines| {
                    assert!(lines.len() == 1, "{name}: the truncated fragment and no phantom after, got {}", lines.len());
                    assert!(
                        lines[0].ends_with(&marker(MAX_LINE_BYTES + 1000)),
                        "{name}: the marker names the whole unterminated line"
                    );
                }),
            ),
            (
                "a newline shared with the next event's start in one chunk keeps it an event",
                // The overhang forces the skip through three full chunks; the fourth holds the
                // line's last bytes, its newline, and the next line's start — one read.
                [
                    vec![b'x'; MAX_LINE_BYTES + 1 + 3 * CHUNK + 5000],
                    format!("\n{after}\n").into_bytes(),
                ]
                .concat(),
                Box::new(move |name, lines| {
                    assert!(lines.len() == 2, "{name}: truncated line + the stashed event, got {}", lines.len());
                    assert!(
                        lines[0].ends_with(&marker(MAX_LINE_BYTES + 1 + 3 * CHUNK + 5000)),
                        "{name}: the marker counts the head and every skipped chunk"
                    );
                    assert_eq!(lines[1], after, "{name}");
                    assert!(is_event(&lines[1]), "{name}: the stashed line still parses as an event");
                }),
            ),
        ];
        for (name, input, check) in cases {
            check(name, &run(&input).await);
        }
    }

    /// Runner output is untrusted in size, so a giant line must be capped on every path off the
    /// runner's pipes: never buffered whole (`for_each_line` reads in chunks and skips past a
    /// line over `MAX_LINE_BYTES`, like cursor.rs), and stored as a bounded excerpt with a
    /// truncation marker naming the line's full size — stderr warn logs through the same
    /// 500-char path as non-event stdout, and a giant "event" fails to parse and takes the
    /// non-event path. A 2 MiB stderr line and a 2 MiB event line must not land whole in
    /// events.jsonl (nor on the /v1/events websockets it feeds), and the stream must stay
    /// aligned: the normal lines after each giant still arrive intact, the one after the stdout
    /// giant still recognised as an event.
    #[tokio::test]
    async fn huge_runner_lines_are_capped() {
        let dir = std::env::temp_dir().join(format!("colonizer-agentd-lines-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(crate::store::EventStore::open(&dir).unwrap());
        let x100 = "x".repeat(100);
        let config: SessionConfig = serde_json::from_value(serde_json::json!({
            "agent": {
                "command": [
                    "sh", "-c",
                    "big=$(dd if=/dev/zero bs=1024 count=2048 2>/dev/null | tr '\\000' x); \
                     printf '%s\\n' \"$big\" >&2; \
                     printf 'stderr after the giant\\n' >&2; \
                     printf '{\"type\":\"log\",\"level\":\"info\",\"message\":\"%s\"}\\n' \"$big\"; \
                     printf '{\"type\":\"log\",\"level\":\"info\",\"message\":\"stdout after the giant\"}\\n'"
                ]
            },
            "workspace": dir.display().to_string(),
        }))
        .unwrap();
        let runner = create();
        runner.launch(&config, store.clone());
        // Wait for the supervisor's "exited" status event, then measure what landed.
        let events_path = dir.join("events.jsonl");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let text = std::fs::read_to_string(&events_path).unwrap_or_default();
            if text.contains("\"state\":\"exited\"") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "runner never exited:\n{text}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let text = std::fs::read_to_string(&events_path).unwrap();
        // (a) Nothing near the giants' 2 MiB landed: every stored line is a bounded excerpt.
        let longest = text.lines().map(str::len).max().unwrap_or(0);
        assert!(
            longest < 4096,
            "a giant line landed whole (or near enough): longest stored line is {longest}"
        );
        // (b) Both giants reached the log truncated, carrying the marker with the line's size.
        let stderr_event = text
            .lines()
            .find(|l| l.contains("\"level\":\"warn\"") && l.contains(&x100));
        assert!(
            stderr_event.is_some_and(|l| l.contains("[truncated 2097152 bytes]")),
            "the 2 MiB stderr line must be stored as a warn with its truncation marker"
        );
        let stdout_event = text
            .lines()
            .find(|l| l.contains("non-event line") && l.contains(&x100));
        assert!(
            stdout_event.is_some_and(|l| l.contains("[truncated ") && l.contains(" bytes]")),
            "the 2 MiB stdout event line must fail to parse and be logged with its marker"
        );
        // (c) The stream stayed aligned on newlines: the lines after the giants arrived whole.
        assert!(
            text.lines().any(|l| l.contains("stderr after the giant") && !l.contains("truncated")),
            "the stderr line after the giant was lost or mangled by the skip"
        );
        let after = text
            .lines()
            .find(|l| l.contains("\"type\":\"log\"") && l.contains("stdout after the giant"));
        assert!(
            after.is_some_and(|l| !l.contains("non-event")),
            "the event after the giant must still parse as an event, not a non-event line"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
