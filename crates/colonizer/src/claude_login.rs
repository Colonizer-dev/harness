//! "Log in with Claude subscription": drives the official `claude setup-token` flow in a
//! pseudo-terminal so the web UI can show the sign-in link and relay the code back. The
//! resulting long-lived OAuth token is saved on the host and never sent to the browser.

use crate::{
    client_error, resolve_host_claude_bin,
    util::{shell_quote, truncate, write_secret},
    ApiResult, Shared,
};
use anyhow::Context;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::{
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{oneshot, Mutex},
};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Serialize)]
pub struct LoginView {
    /// idle | starting | awaiting_code | verifying | done | error
    state: &'static str,
    url: Option<String>,
    message: Option<String>,
}

impl LoginView {
    fn new(state: &'static str) -> Self {
        Self { state, url: None, message: None }
    }
}

struct Session {
    id: u64,
    view: LoginView,
    stdin: Option<ChildStdin>,
    /// Dropping this (by replacing or clearing the session) stops the driver task.
    _cancel: oneshot::Sender<()>,
}

#[derive(Default)]
pub struct LoginManager {
    session: Mutex<Option<Session>>,
    seq: AtomicU64,
}

pub async fn status(State(app): State<Shared>) -> Json<LoginView> {
    let session = app.login.session.lock().await;
    Json(session.as_ref().map_or_else(|| LoginView::new("idle"), |s| s.view.clone()))
}

pub async fn start(State(app): State<Shared>) -> ApiResult<LoginView> {
    let bin = resolve_host_claude_bin(&app.cfg).await?;
    // A very wide terminal keeps the sign-in URL and the token on single lines.
    let script = format!("stty cols 4000 rows 60; exec {} setup-token", shell_quote(&bin.display().to_string()));
    // util-linux and BSD `script` disagree on everything but the name: the command is `-c CMD FILE`
    // there and `FILE CMD...` here, and unbuffered output is `-f` there and `-F` here.
    let mut cmd = Command::new("script");
    let args: &[&str] = if cfg!(target_os = "macos") {
        &["-qFe", "/dev/null", "sh", "-c", script.as_str()]
    } else {
        &["-qfec", script.as_str(), "/dev/null"]
    };
    cmd.args(args)
        // Don't pop a browser on the host; the web UI shows the link instead.
        .env("BROWSER", "true")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (key, _) in std::env::vars() {
        if key == "CLAUDECODE" || key.starts_with("CLAUDE_CODE_") {
            cmd.env_remove(key);
        }
    }
    let mut child = cmd.spawn().context("failed to start `script` (util-linux) for claude setup-token")?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().context("no output from claude setup-token")?;

    let id = app.login.seq.fetch_add(1, Ordering::SeqCst);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let view = LoginView::new("starting");
    *app.login.session.lock().await = Some(Session { id, view: view.clone(), stdin, _cancel: cancel_tx });
    tokio::spawn(drive(app.clone(), id, child, stdout, cancel_rx));
    Ok(Json(view))
}

#[derive(Deserialize)]
pub struct CodeBody {
    code: String,
}

pub async fn submit_code(State(app): State<Shared>, Json(body): Json<CodeBody>) -> ApiResult<LoginView> {
    let code = body.code.trim();
    // Printable ASCII only, so the code can't smuggle extra keystrokes into the terminal.
    if code.is_empty() || code.len() > 2000 || !code.chars().all(|c| c.is_ascii_graphic()) {
        return Err(client_error(StatusCode::BAD_REQUEST, "that doesn't look like a sign-in code"));
    }
    let mut guard = app.login.session.lock().await;
    let session = guard
        .as_mut()
        .filter(|s| s.view.state == "awaiting_code")
        .ok_or_else(|| client_error(StatusCode::CONFLICT, "no Claude sign-in is waiting for a code"))?;
    let stdin = session
        .stdin
        .as_mut()
        .ok_or_else(|| client_error(StatusCode::CONFLICT, "the sign-in process has ended"))?;
    stdin.write_all(code.as_bytes()).await?;
    stdin.flush().await?;
    // Send Enter on its own so the prompt doesn't read it as part of a paste.
    tokio::time::sleep(Duration::from_millis(300)).await;
    stdin.write_all(b"\r").await?;
    stdin.flush().await?;
    session.view.state = "verifying";
    session.view.message = None;
    Ok(Json(session.view.clone()))
}

pub async fn cancel(State(app): State<Shared>) -> Json<LoginView> {
    app.login.session.lock().await.take();
    Json(LoginView::new("idle"))
}

async fn drive(app: Shared, id: u64, mut child: Child, mut stdout: ChildStdout, mut cancel: oneshot::Receiver<()>) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    let deadline = tokio::time::sleep(LOGIN_TIMEOUT);
    tokio::pin!(deadline);

    let outcome: Result<String, String> = loop {
        tokio::select! {
            read = stdout.read(&mut buf) => {
                let n = match read {
                    Ok(0) | Err(_) => break Err(exit_message(&raw)),
                    Ok(n) => n,
                };
                if raw.len() < 4 << 20 {
                    raw.extend_from_slice(&buf[..n]);
                }
                let text = strip_ansi(&String::from_utf8_lossy(&raw));
                if let Some(token) = find_token(&text) {
                    break Ok(token);
                }
                let mut guard = app.login.session.lock().await;
                let Some(session) = guard.as_mut().filter(|s| s.id == id) else {
                    break Err("superseded".into());
                };
                if session.view.url.is_none() {
                    if let Some(url) = find_sign_in_url(&text) {
                        session.view.url = Some(url);
                        session.view.state = "awaiting_code";
                    }
                }
            }
            _ = &mut cancel => break Err("cancelled".into()),
            _ = &mut deadline => break Err("timed out waiting for the Claude sign-in".into()),
        }
    };

    kill_tree(&mut child).await;

    let mut guard = app.login.session.lock().await;
    let Some(session) = guard.as_mut().filter(|s| s.id == id) else { return };
    session.stdin = None;
    session.view = match outcome {
        Ok(token) => match write_secret(&app.claude_token_file(), &token) {
            Ok(()) => LoginView {
                state: "done",
                url: None,
                message: Some("Connected your Claude subscription".into()),
            },
            Err(e) => LoginView { state: "error", url: None, message: Some(format!("could not save the token: {e:#}")) },
        },
        Err(message) => LoginView { state: "error", url: None, message: Some(redact(&message)) },
    };
}

/// `script` starts claude in its own session, so killing `script` alone would leave it running.
async fn kill_tree(child: &mut Child) {
    if let Some(pid) = child.id() {
        let mut descendants = Vec::new();
        collect_descendants(pid, &mut descendants);
        if !descendants.is_empty() {
            let _ = Command::new("kill")
                .arg("-KILL")
                .args(descendants.iter().map(u32::to_string))
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Every descendant of `pid`. `/proc` is Linux-only; `ps` tells the same story on macOS too.
fn collect_descendants(pid: u32, out: &mut Vec<u32>) {
    let Ok(output) = std::process::Command::new("ps").args(["-Ao", "pid=,ppid="]).output() else { return };
    let text = String::from_utf8_lossy(&output.stdout);
    let pairs: Vec<(u32, u32)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut stack = vec![pid];
    while let Some(parent) = stack.pop() {
        for (child, _) in pairs.iter().filter(|(_, ppid)| *ppid == parent) {
            if !out.contains(child) {
                out.push(*child);
                stack.push(*child);
            }
        }
    }
}

/// Removes terminal escape sequences, turning cursor-forward moves back into spaces.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    while let Some(p) = chars.next() {
                        if ('\u{40}'..='\u{7e}').contains(&p) {
                            if p == 'C' {
                                let n = params.parse::<usize>().unwrap_or(1).min(200);
                                out.extend(std::iter::repeat_n(' ', n));
                            }
                            break;
                        }
                        params.push(p);
                    }
                }
                Some(']') => {
                    while let Some(p) = chars.next() {
                        if p == '\u{7}' {
                            break;
                        }
                        if p == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Returns the first complete sign-in URL (one that is followed by whitespace).
fn find_sign_in_url(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(pos) = rest.find("https://") {
        let candidate = &rest[pos..];
        let end = candidate.find(char::is_whitespace)?;
        let url = &candidate[..end];
        if url.contains("/oauth/authorize?") {
            return Some(url.to_string());
        }
        rest = &candidate[end..];
    }
    None
}

/// Returns the first complete OAuth token. A token running to the end of the buffer may still be
/// arriving, so it only counts once something follows it.
fn find_token(text: &str) -> Option<String> {
    let is_token_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut rest = text;
    while let Some(pos) = rest.find("sk-ant-oat") {
        let candidate = &rest[pos..];
        match candidate.find(|c: char| !is_token_char(c)) {
            Some(len) if len >= 40 => return Some(candidate[..len].to_string()),
            Some(_) => rest = &candidate[1..],
            None => return None,
        }
    }
    None
}

fn exit_message(raw: &[u8]) -> String {
    let text = strip_ansi(&String::from_utf8_lossy(raw));
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| line.chars().count() > 3 && !line.starts_with("Paste code"))
        .map(|line| format!("claude setup-token ended: {}", truncate(line, 300)))
        .unwrap_or_else(|| "claude setup-token ended without producing a token".into())
}

fn redact(message: &str) -> String {
    match message.find("sk-ant-") {
        Some(pos) => format!("{}sk-ant-…", &message[..pos]),
        None => message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_sign_in_url_from_terminal_output() {
        let raw = "\u{1b}[?25l\u{1b}[2KBrowser\u{1b}[1Cdidn't\u{1b}[1Copen?\r\n\
                   https://claude.com/cai/oauth/authorize?code=true&client_id=abc&state=xyz\r\n\
                   \u{1b}]8;;\u{7}Paste code here if prompted >";
        let text = strip_ansi(raw);
        assert!(text.contains("Browser didn't open?"));
        assert_eq!(
            find_sign_in_url(&text).as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&client_id=abc&state=xyz")
        );
        assert_eq!(find_token(&text), None);
    }

    #[test]
    fn url_is_not_reported_until_complete() {
        assert_eq!(find_sign_in_url("https://claude.com/cai/oauth/authorize?code=tr"), None);
    }

    #[test]
    fn token_is_only_accepted_once_fully_printed() {
        let token = format!("sk-ant-oat01-{}", "A1_b-".repeat(20));
        assert_eq!(find_token(&strip_ansi(&format!("Your OAuth token:\n\u{1b}[1m{token}\u{1b}[22m\n"))), Some(token.clone()));
        assert_eq!(find_token(&format!("Your OAuth token:\n{}", &token[..50])), None);
    }

    #[test]
    fn redacts_tokens_in_messages() {
        assert_eq!(redact("failed near sk-ant-oat01-secret"), "failed near sk-ant-…");
    }
}
