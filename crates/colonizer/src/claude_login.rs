//! "Log in with Claude subscription": drives the official `claude setup-token` flow in a
//! pseudo-terminal so the web UI can show the sign-in link and relay the code back. The
//! resulting long-lived OAuth token is saved on the host and never sent to the browser.

use crate::{
    ApiResult, App, ClaudeCred, Shared, client_error, resolve_host_claude_bin, secrets,
    util::{fingerprint, shell_quote, truncate},
};
use anyhow::Context;
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, oneshot},
};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Asks which account an OAuth credential belongs to. A `claude setup-token` only carries the
/// `user:inference` scope, so a 403 here is the expected answer for a subscription token.
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Both answers and failures are kept this long, so the status poll (every 30 s per open tab)
/// never hammers Anthropic with a lookup that is going to fail again.
const ACCOUNT_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Serialize)]
pub struct LoginView {
    /// idle | starting | awaiting_code | verifying | done | error
    state: &'static str,
    url: Option<String>,
    message: Option<String>,
}

impl LoginView {
    fn new(state: &'static str) -> Self {
        Self {
            state,
            url: None,
            message: None,
        }
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
    let bin = resolve_host_claude_bin(&app).await?;
    // A very wide terminal keeps the sign-in URL and the token on single lines.
    let script = format!(
        "stty cols 4000 rows 60; exec {} setup-token",
        shell_quote(&bin.display().to_string())
    );
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
    let mut child = cmd
        .spawn()
        .context("failed to start `script` (util-linux) for claude setup-token")?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().context("no output from claude setup-token")?;

    let id = app.login.seq.fetch_add(1, Ordering::SeqCst);
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let view = LoginView::new("starting");
    *app.login.session.lock().await = Some(Session {
        id,
        view: view.clone(),
        stdin,
        _cancel: cancel_tx,
    });
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
                if session.view.url.is_none()
                    && let Some(url) = find_sign_in_url(&text)
                {
                    session.view.url = Some(url);
                    session.view.state = "awaiting_code";
                }
            }
            _ = &mut cancel => break Err("cancelled".into()),
            _ = &mut deadline => break Err("timed out waiting for the Claude sign-in".into()),
        }
    };

    kill_tree(&mut child).await;

    let mut guard = app.login.session.lock().await;
    let Some(session) = guard.as_mut().filter(|s| s.id == id) else {
        return;
    };
    session.stdin = None;
    session.view = match outcome {
        Ok(token) => match secrets::write_secret_value(&app.claude_token_file(), &token) {
            Ok(()) => LoginView {
                state: "done",
                url: None,
                message: Some("Connected your Claude subscription".into()),
            },
            Err(e) => LoginView {
                state: "error",
                url: None,
                message: Some(format!("could not save the token: {e:#}")),
            },
        },
        Err(message) => LoginView {
            state: "error",
            url: None,
            message: Some(redact(&message)),
        },
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
    let Ok(output) = std::process::Command::new("ps").args(["-Ao", "pid=,ppid="]).output() else {
        return;
    };
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
                    for p in chars.by_ref() {
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
                    for p in chars.by_ref() {
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

// ---------------------------------------------------------------------------
// Claude account identity for the status payload
// ---------------------------------------------------------------------------

/// The last Anthropic profile lookup, cached on `App` so the status poll does not repeat it. Keyed on
/// a fingerprint of the token — the token itself is never stored, logged or sent to the browser.
#[derive(Clone)]
pub struct AccountStatus {
    fingerprint: String,
    looked_up_at: Instant,
    /// The identity to show (an email when Anthropic names one).
    account: Option<String>,
    /// Set when `account` is None, saying plainly why, so the UI never shows an empty row.
    account_note: Option<String>,
    /// A real expiry the profile response carried, if any (the response shape is not documented).
    profile_expires_at: Option<String>,
}

/// Why the profile lookup did not produce an account.
enum ProfileError {
    /// 403: the token's scope (`user:inference`) does not include the profile's `user:profile`.
    Forbidden,
    /// 401: the token was rejected — expired or revoked.
    Unauthorized,
    /// Anything else, with a short reason (routed through `redact` before it leaves this module).
    Other(String),
}

/// The `claude` object of `GET /api/status`. These field names are the JSON contract the web UI is
/// written against. With nothing configured the identity fields are null and `expires_estimated` false.
pub async fn claude_status(app: &App, cred: Option<&ClaudeCred>) -> Value {
    let Some(cred) = cred else {
        return json!({
            "configured": false, "source": null, "kind": null, "account": null,
            "account_note": null, "saved_at": null, "expires_at": null, "expires_estimated": false,
        });
    };
    let saved = saved_at(app, cred);
    let status = account_status(app, cred).await;
    // A saved subscription token is valid for a year from the moment it was minted, and nothing
    // records that moment, so the best honest estimate is saving time plus 365 days.
    let (expires_at, expires_estimated) = match (&status.profile_expires_at, saved) {
        (Some(at), _) => (Some(at.clone()), false),
        (None, Some(saved)) if !is_api_key(cred) => (Some(estimated_expiry(saved).to_rfc3339()), true),
        (None, _) => (None, false),
    };
    json!({
        "configured": true,
        "source": cred.source,
        "kind": cred.env,
        "account": status.account,
        "account_note": status.account_note,
        "saved_at": saved.map(|at| at.to_rfc3339()),
        "expires_at": expires_at,
        "expires_estimated": expires_estimated,
    })
}

fn is_api_key(cred: &ClaudeCred) -> bool {
    cred.value.starts_with("sk-ant-api")
}

/// The cached lookup. The cache lock is held across the request so concurrent status polls share one
/// lookup instead of stacking several; the request itself is bounded by `fetch_profile`'s timeout.
async fn account_status(app: &App, cred: &ClaudeCred) -> AccountStatus {
    let fingerprint = fingerprint(&cred.value);
    if is_api_key(cred) {
        // An API key carries no account identity and there is no endpoint to ask, so don't.
        return AccountStatus {
            fingerprint,
            looked_up_at: Instant::now(),
            account: None,
            account_note: Some("an API key does not identify an account".into()),
            profile_expires_at: None,
        };
    }
    let mut cache = app.claude_account.lock().await;
    if let Some(cached) = cache.as_ref()
        && cached.fingerprint == fingerprint
        && cached.looked_up_at.elapsed() < ACCOUNT_CACHE_TTL
    {
        return cached.clone();
    }
    let fresh = match fetch_profile(&cred.value).await {
        Ok(profile) => {
            let account = account_label(&profile);
            let note = account
                .is_none()
                .then(|| "Anthropic answered, but the profile did not name an account".into());
            AccountStatus {
                fingerprint,
                looked_up_at: Instant::now(),
                account,
                account_note: note,
                profile_expires_at: profile_expires_at(&profile),
            }
        }
        Err(ProfileError::Forbidden) => AccountStatus {
            fingerprint,
            looked_up_at: Instant::now(),
            account: None,
            account_note: Some(
                "this token is only allowed to make model requests, so Anthropic will not say which account it belongs to".into(),
            ),
            profile_expires_at: None,
        },
        Err(ProfileError::Unauthorized) => AccountStatus {
            fingerprint,
            looked_up_at: Instant::now(),
            account: None,
            account_note: Some("the token was rejected — it may have expired or been revoked".into()),
            profile_expires_at: None,
        },
        Err(ProfileError::Other(reason)) => AccountStatus {
            fingerprint,
            looked_up_at: Instant::now(),
            account: None,
            account_note: Some(format!(
                "could not reach Anthropic to check ({})",
                redact(&truncate(&reason, 200))
            )),
            profile_expires_at: None,
        },
    };
    *cache = Some(fresh.clone());
    fresh
}

/// Asks Anthropic which account the token belongs to. Bounded to five seconds so `/api/status`, which
/// is polled, can never hang on it.
async fn fetch_profile(token: &str) -> Result<Value, ProfileError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| ProfileError::Other(format!("could not build an HTTP client: {e}")))?;
    let response = client
        .get(PROFILE_URL)
        .bearer_auth(token)
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await
        .map_err(|e| ProfileError::Other(format!("{e}")))?;
    match response.status().as_u16() {
        200 => response
            .json()
            .await
            .map_err(|e| ProfileError::Other(format!("unreadable profile: {e}"))),
        403 => Err(ProfileError::Forbidden),
        401 => Err(ProfileError::Unauthorized),
        code => Err(ProfileError::Other(format!("Anthropic answered {code}"))),
    }
}

/// Pulls a display identity out of the profile response. The exact shape is not documented, so try the
/// likely paths and fall back to the organisation's name; none of them match, there is no identity.
fn account_label(profile: &Value) -> Option<String> {
    [
        profile["account"]["email_address"].as_str(),
        profile["account"]["email"].as_str(),
        profile["email_address"].as_str(),
        profile["email"].as_str(),
        profile["organization"]["name"].as_str(),
    ]
    .into_iter()
    .flatten()
    .find(|s| !s.is_empty())
    .map(String::from)
}

/// An expiry the profile response happens to carry, as a string. The shape is not documented, so try
/// the likely fields and otherwise say there is none rather than guess.
fn profile_expires_at(profile: &Value) -> Option<String> {
    [profile["expires_at"].as_str(), profile["account"]["expires_at"].as_str()]
        .into_iter()
        .flatten()
        .next()
        .map(String::from)
}

/// When this harness saved the credential: the token file's mtime. The token itself carries no dates,
/// and a credential from the environment has no file, so there is nothing to show for one.
fn saved_at(app: &App, cred: &ClaudeCred) -> Option<DateTime<Utc>> {
    if !matches!(cred.source, "saved API key" | "Claude subscription") {
        return None;
    }
    let modified = std::fs::metadata(app.claude_token_file()).ok()?.modified().ok()?;
    Some(DateTime::from(modified))
}

/// `claude setup-token` mints a token that is valid for one year.
fn estimated_expiry(saved: DateTime<Utc>) -> DateTime<Utc> {
    saved + chrono::Duration::days(365)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
        assert_eq!(
            find_token(&strip_ansi(&format!("Your OAuth token:\n\u{1b}[1m{token}\u{1b}[22m\n"))),
            Some(token.clone())
        );
        assert_eq!(find_token(&format!("Your OAuth token:\n{}", &token[..50])), None);
    }

    #[test]
    fn redacts_tokens_in_messages() {
        assert_eq!(redact("failed near sk-ant-oat01-secret"), "failed near sk-ant-…");
    }

    #[test]
    fn account_label_reads_email_from_the_nested_account_object() {
        let profile = serde_json::json!({"account": {"email_address": "ada@example.com"}});
        assert_eq!(account_label(&profile).as_deref(), Some("ada@example.com"));
        let profile = serde_json::json!({"account": {"email": "ada@example.com"}});
        assert_eq!(account_label(&profile).as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn account_label_reads_email_from_the_top_level() {
        let profile = serde_json::json!({"email_address": "ada@example.com"});
        assert_eq!(account_label(&profile).as_deref(), Some("ada@example.com"));
        let profile = serde_json::json!({"email": "ada@example.com"});
        assert_eq!(account_label(&profile).as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn account_label_falls_back_to_the_organization_name() {
        let profile = serde_json::json!({"organization": {"name": "Acme Rockets"}});
        assert_eq!(account_label(&profile).as_deref(), Some("Acme Rockets"));
        // An email wins when both are present.
        let profile = serde_json::json!({"email": "ada@example.com", "organization": {"name": "Acme Rockets"}});
        assert_eq!(account_label(&profile).as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn account_label_gives_up_on_an_unrecognised_shape() {
        assert_eq!(account_label(&serde_json::json!({})), None);
        assert_eq!(account_label(&serde_json::json!({"account": {"name": "Ada"}})), None);
        assert_eq!(account_label(&serde_json::json!({"email": ""})), None);
        assert_eq!(account_label(&serde_json::json!({"email": 42})), None);
    }

    #[test]
    fn profile_expiry_is_read_from_either_level() {
        let profile = serde_json::json!({"expires_at": "2027-09-17T00:00:00Z"});
        assert_eq!(profile_expires_at(&profile).as_deref(), Some("2027-09-17T00:00:00Z"));
        let profile = serde_json::json!({"account": {"expires_at": "2027-09-17T00:00:00Z"}});
        assert_eq!(profile_expires_at(&profile).as_deref(), Some("2027-09-17T00:00:00Z"));
        assert_eq!(profile_expires_at(&serde_json::json!({"sub": "abc"})), None);
        assert_eq!(profile_expires_at(&serde_json::json!({})), None);
    }

    #[test]
    fn estimated_expiry_is_one_year_after_saving() {
        let saved = Utc.with_ymd_and_hms(2026, 9, 17, 12, 0, 0).unwrap();
        assert_eq!(estimated_expiry(saved), Utc.with_ymd_and_hms(2027, 9, 17, 12, 0, 0).unwrap());
        // It is 365 days, not the calendar anniversary: a leap day inside the window pulls the
        // estimate back a day, which is the honest reading of "valid for 1 year" at 365 days.
        let saved = Utc.with_ymd_and_hms(2023, 3, 1, 0, 0, 0).unwrap();
        assert_eq!(estimated_expiry(saved), Utc.with_ymd_and_hms(2024, 2, 29, 0, 0, 0).unwrap());
    }

    #[test]
    fn fingerprint_is_stable_and_never_carries_the_token() {
        let token = "sk-ant-oat01-ABCDEF0123456789abcdef";
        assert_eq!(fingerprint(token), fingerprint(token));
        assert_ne!(fingerprint(token), fingerprint("sk-ant-oat01-ABCDEF0123456789abcdeg"));
        assert!(!fingerprint(token).contains("ABCDEF"));
    }
}
