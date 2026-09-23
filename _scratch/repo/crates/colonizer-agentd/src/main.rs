//! colonizer-agentd: the daemon inside every Colonizer microVM. It runs the agent runner module, keeps the
//! session event log, and serves events, terminals and shutdown to the harness over the mesh.
//! Contract: docs/protocol.md §1–§3.

mod config;
mod pty;
mod runner;
mod store;

use axum::{
    Json, Router,
    extract::{
        Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, process::ExitCode, sync::Arc};
use tokio::sync::broadcast::error::RecvError;

use crate::{
    config::SessionConfig,
    runner::Runner,
    store::{EventStore, log_event},
};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const USAGE: &str = "usage: colonizer-agentd [--config PATH] [--token-file PATH] [--state-dir DIR]

  --config PATH      session config            (default /colonizer/session.json)
  --token-file PATH  bearer token for the API  (default /colonizer/token)
  --state-dir DIR    event log directory       (default /var/lib/colonizer)
  --version          print the version";

struct Args {
    config: PathBuf,
    token_file: PathBuf,
    state_dir: PathBuf,
}

impl Args {
    /// `Ok(None)` means the command was fully handled (`--help`, `--version`).
    fn parse(argv: Vec<String>) -> Result<Option<Self>, String> {
        let mut args = Args {
            config: "/colonizer/session.json".into(),
            token_file: "/colonizer/token".into(),
            state_dir: "/var/lib/colonizer".into(),
        };
        let mut iter = argv.into_iter();
        while let Some(arg) = iter.next() {
            let split = arg
                .split_once('=')
                .filter(|(flag, _)| flag.starts_with("--"))
                .map(|(flag, value)| (flag.to_string(), value.to_string()));
            let (flag, inline) = match split {
                Some((flag, value)) => (flag, Some(value)),
                None => (arg, None),
            };
            match flag.as_str() {
                "--version" | "-V" => {
                    println!("colonizer-agentd {VERSION}");
                    return Ok(None);
                }
                "--help" | "-h" => {
                    println!("{USAGE}");
                    return Ok(None);
                }
                "--config" | "--token-file" | "--state-dir" => {
                    let value = match inline {
                        Some(value) => value,
                        None => iter.next().ok_or_else(|| format!("{flag} needs a value"))?,
                    };
                    let slot = match flag.as_str() {
                        "--config" => &mut args.config,
                        "--token-file" => &mut args.token_file,
                        _ => &mut args.state_dir,
                    };
                    *slot = PathBuf::from(value);
                }
                _ => return Err(format!("unknown argument: {flag}")),
            }
        }
        Ok(Some(args))
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1).collect()) {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("colonizer-agentd: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("colonizer-agentd: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), BoxError> {
    let raw = std::fs::read(&args.config).map_err(|e| format!("cannot read {}: {e}", args.config.display()))?;
    let config: SessionConfig = serde_json::from_slice(&raw).map_err(|e| format!("invalid {}: {e}", args.config.display()))?;
    let token = std::fs::read_to_string(&args.token_file)
        .map_err(|e| format!("cannot read {}: {e}", args.token_file.display()))?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(format!("{} is empty", args.token_file.display()).into());
    }
    let store = Arc::new(
        EventStore::open(&args.state_dir).map_err(|e| format!("cannot open event log in {}: {e}", args.state_dir.display()))?,
    );
    // Bind before starting the runner so a bad listen address fails fast.
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    eprintln!(
        "colonizer-agentd {VERSION}: session {} (agent {}) listening on {}",
        config.session_id, config.agent.module, config.listen
    );
    store.append(log_event(
        "info",
        format!(
            "colonizer-agentd {VERSION} listening on {} (agent module {})",
            config.listen, config.agent.module
        ),
    ));

    let runner = runner::start(&config, store.clone());
    let state = AppState {
        store,
        runner: runner.clone(),
        token: Arc::from(token),
        workspace: config.workspace,
    };
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/events", get(events))
        .route("/v1/pty", get(pty_socket))
        .route("/v1/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state);

    tokio::select! {
        result = async { axum::serve(listener, app).await } => result?,
        _ = shutdown_signal() => {
            eprintln!("colonizer-agentd: stopping the agent runner");
            runner.shutdown().await;
        }
    }
    Ok(())
}

#[derive(Clone)]
struct AppState {
    store: Arc<EventStore>,
    runner: Arc<Runner>,
    token: Arc<str>,
    workspace: PathBuf,
}

async fn require_token(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.trim().as_bytes(), state.token.as_bytes()));
    if authorized {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response()
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "version": VERSION,
        "agent": {
            "state": state.store.agent_state(),
            "running": state.runner.is_running(),
            "last_seq": state.store.last_seq(),
        },
    }))
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    since: u64,
}

async fn events(State(state): State<AppState>, Query(query): Query<EventsQuery>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| stream_events(socket, state, query.since))
}

async fn stream_events(socket: WebSocket, state: AppState, since: u64) {
    let (mut sink, mut incoming) = socket.split();
    let (snapshot, mut live) = state.store.subscribe();

    let mut commands = tokio::spawn({
        let state = state.clone();
        async move {
            while let Some(Ok(message)) = incoming.next().await {
                match message {
                    Message::Text(text) => forward_command(&state, &text),
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    });

    let outgoing = async {
        let mut sent = since;
        for event in state.store.replay(since, snapshot).await {
            if sink.send(Message::Text(event.line.into())).await.is_err() {
                return;
            }
            sent = event.seq;
        }
        sent = sent.max(snapshot);
        loop {
            match live.recv().await {
                Ok(event) => {
                    if event.seq <= sent {
                        continue;
                    }
                    if sink.send(Message::Text(event.line.clone().into())).await.is_err() {
                        return;
                    }
                    sent = event.seq;
                }
                Err(RecvError::Lagged(_)) => {
                    // This client fell behind the broadcast buffer; catch up from the log.
                    let upto = state.store.last_seq();
                    for event in state.store.replay(sent, upto).await {
                        if sink.send(Message::Text(event.line.into())).await.is_err() {
                            return;
                        }
                        sent = event.seq;
                    }
                }
                Err(RecvError::Closed) => return,
            }
        }
    };

    tokio::select! {
        _ = outgoing => {}
        _ = &mut commands => {}
    }
    commands.abort();
}

fn forward_command(state: &AppState, text: &str) {
    let Ok(command @ Value::Object(_)) = serde_json::from_str::<Value>(text) else {
        return;
    };
    let kind = command["type"].as_str().unwrap_or_default();
    if !matches!(kind, "user_message" | "answer" | "interrupt" | "set_model") {
        return;
    }
    if !state.runner.send(&command) {
        state.store.append(log_event(
            "warn",
            format!("agent runner is not running; dropped `{kind}` command"),
        ));
    }
}

#[derive(Deserialize)]
struct PtyQuery {
    cols: Option<u16>,
    rows: Option<u16>,
}

async fn pty_socket(State(state): State<AppState>, Query(query): Query<PtyQuery>, upgrade: WebSocketUpgrade) -> Response {
    let cols = query.cols.unwrap_or(80).clamp(1, 1000);
    let rows = query.rows.unwrap_or(24).clamp(1, 1000);
    upgrade.on_upgrade(move |socket| pty::serve(socket, state.workspace, cols, rows))
}

async fn shutdown(State(state): State<AppState>) -> Json<Value> {
    state.runner.shutdown().await;
    Json(json!({"ok": true}))
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}
