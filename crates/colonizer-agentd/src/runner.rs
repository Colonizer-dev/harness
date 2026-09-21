//! The agent runner module process (docs/protocol.md §2): commands on stdin, events on stdout,
//! stderr lines become `log` events.

use crate::{
    config::SessionConfig,
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
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
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

pub struct Runner {
    commands: mpsc::UnboundedSender<String>,
    running: watch::Receiver<bool>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl Runner {
    pub fn is_running(&self) -> bool {
        *self.running.borrow()
    }

    /// Queues a command for the runner's stdin; false when no runner is alive to receive it.
    pub fn send(&self, command: &Value) -> bool {
        self.is_running() && self.commands.send(command.to_string()).is_ok()
    }

    /// Asks the runner to exit, then kills it if it hasn't within 10 s.
    pub async fn shutdown(&self) {
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

pub fn start(config: &SessionConfig, store: Arc<EventStore>) -> Arc<Runner> {
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (running_tx, running) = watch::channel(false);
    let (kill_tx, kill_rx) = oneshot::channel();
    let runner = Arc::new(Runner {
        commands,
        running,
        kill: Mutex::new(Some(kill_tx)),
    });

    let Some((program, args)) = config.agent.command.split_first() else {
        store.append(status_event("error", Some("agent.command is empty".into())));
        return runner;
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
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            store.append(log_event("error", format!("cannot start agent runner `{program}`: {e}")));
            store.append(status_event("error", Some(spawn_failure_detail(program, &e))));
            return runner;
        }
    };
    running_tx.send_replace(true);

    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        unreachable!("runner stdio is piped");
    };
    tokio::spawn(feed_stdin(stdin, command_rx));
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
        for_each_line(stderr, move |line| {
            store.append(log_event("warn", line));
        })
    });

    if let Some(prompt) = config.initial_prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        runner.send(&json!({"type": "user_message", "id": "initial", "text": prompt}));
    }

    tokio::spawn(async move {
        let status = tokio::select! {
            status = child.wait() => status,
            Ok(()) = kill_rx => {
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
        running_tx.send_replace(false);
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

    runner
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
async fn for_each_line(reader: impl AsyncRead + Unpin, mut f: impl FnMut(&str)) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim();
                if !line.is_empty() {
                    f(line);
                }
            }
        }
    }
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
}
