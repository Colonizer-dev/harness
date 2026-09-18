//! The notify module: when a colony needs an answer, stalls, fails or opens a pull request, say so
//! on the desktop the mothership runs on and, if a URL is set, at a webhook.
//!
//! Two choices shape it. Events are detected by diffing the session list in a poll loop, never by
//! hooking the call sites — a notification must not be able to change what a colony does, and a new
//! status change elsewhere cannot forget to tell it. And what leaves the mothership is one short
//! line about the colony, never repository content: a webhook is a write to somewhere outside this
//! machine, and repository content can carry instructions.

use crate::{
    ApiResult, App, Shared, client_error,
    orgs::effective_notify,
    sessions::{Session, SessionStatus},
    util::{env_nonempty, read_trimmed, truncate, write_secret},
};
use anyhow::{Result, bail};
use axum::{Json, extract::State, http::StatusCode};
use chrono::{DateTime, Utc};
use ring::hmac;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, process::Stdio, time::Duration};

/// The most characters one notification carries: repository, issue number and a few words. A desktop
/// popup has no use for more, and neither does a webhook note.
const MAX_TEXT: usize = 200;

/// What to announce and where, resolved from the module settings plus an org's overrides.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotifySettings {
    pub enabled: bool,
    pub on_question: bool,
    pub on_attention: bool,
    pub on_failed: bool,
    pub on_pull_request: bool,
    pub desktop: bool,
    pub webhook_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `status` became `waiting_for_answer`.
    Question,
    /// The watchdog's `attention.reason` became the named reason (`stalled` or `nudges_exhausted`).
    /// Autopilot's `autopilot_held` and the watchdog's own `waiting_for_answer` are not here: the
    /// first is not the watchdog's, the second is this module's `question`.
    Attention(&'static str),
    /// `status` became `failed`.
    Failed,
    /// `status` became `pr_opened`.
    PullRequest,
}

impl Event {
    /// The event's name in the webhook payload.
    pub fn name(self) -> &'static str {
        match self {
            Event::Question => "question",
            Event::Attention(_) => "attention",
            Event::Failed => "failed",
            Event::PullRequest => "pull_request",
        }
    }

    /// The one line both channels carry: the colony, and what happened. No issue title, no question
    /// text, no branch — for the webhook this line is all of what leaves the mothership.
    pub fn text(self, repo: &str, issue: Option<u64>) -> String {
        let what = match self {
            Event::Question => "needs an answer",
            Event::Attention("nudges_exhausted") => "is out of nudges",
            Event::Attention(_) => "has stalled",
            Event::Failed => "failed",
            Event::PullRequest => "opened a pull request",
        };
        let subject = match issue {
            Some(number) => format!("{repo} #{number}"),
            None => repo.to_string(),
        };
        truncate(&format!("{subject} {what}"), MAX_TEXT)
    }
}

/// What was last seen of a colony: the whole of what edge detection needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub status: SessionStatus,
    pub attention: Option<String>,
}

impl Seen {
    fn of(session: &Session) -> Self {
        Self {
            status: session.status,
            attention: session
                .attention
                .as_ref()
                .and_then(|a| a["reason"].as_str())
                .map(String::from),
        }
    }
}

/// The events a colony's change since it was last seen calls for. A colony seen for the first time
/// only seeds: a restart must not replay a backlog, and neither should a colony's first appearance
/// count as a change.
pub fn decide(settings: &NotifySettings, last: Option<&Seen>, now: &Seen) -> Vec<Event> {
    if !settings.enabled {
        return Vec::new();
    }
    let Some(last) = last else { return Vec::new() };
    let became = |status: SessionStatus| now.status == status && last.status != status;
    let mut events = Vec::new();
    if settings.on_question && became(SessionStatus::WaitingForAnswer) {
        events.push(Event::Question);
    }
    if settings.on_attention {
        for reason in ["stalled", "nudges_exhausted"] {
            if now.attention.as_deref() == Some(reason) && last.attention.as_deref() != Some(reason) {
                events.push(Event::Attention(reason));
            }
        }
    }
    if settings.on_failed && became(SessionStatus::Failed) {
        events.push(Event::Failed);
    }
    if settings.on_pull_request && became(SessionStatus::PrOpened) {
        events.push(Event::PullRequest);
    }
    events
}

/// Diffs the session list against what was last seen: the events to announce, and the state to keep.
/// Entries for colonies that are gone are simply not carried over, so the map cannot grow forever.
fn diff<'a>(
    sessions: &'a [Session],
    seen: &HashMap<String, Seen>,
    mut settings_for: impl FnMut(&Session) -> NotifySettings,
) -> (Vec<(&'a Session, Event)>, HashMap<String, Seen>) {
    let mut next = HashMap::with_capacity(sessions.len());
    let mut events = Vec::new();
    for session in sessions {
        let now = Seen::of(session);
        for event in decide(&settings_for(session), seen.get(&session.id), &now) {
            events.push((session, event));
        }
        next.insert(session.id.clone(), now);
    }
    (events, next)
}

// ---------------------------------------------------------------------------
// The desktop channel
// ---------------------------------------------------------------------------

/// The command the desktop channel runs, when there is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// macOS, through AppleScript.
    Osascript,
    /// Linux desktop notifications.
    NotifySend,
}

/// The inputs the desktop decision reads, passed in rather than read from the process so the
/// decision can be tested without pretending to be another machine.
#[derive(Clone, Debug, Default)]
pub struct DesktopEnv {
    pub os: &'static str,
    pub osascript_on_path: bool,
    pub notify_send_on_path: bool,
    pub display: Option<String>,
    pub wayland_display: Option<String>,
    pub ssh_connection: Option<String>,
    pub ssh_tty: Option<String>,
}

impl DesktopEnv {
    /// This machine, as the decision sees it.
    fn this_host() -> Self {
        let on_path = |name: &str| {
            std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).any(|dir| dir.join(name).exists()))
                .unwrap_or(false)
        };
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        Self {
            os: std::env::consts::OS,
            osascript_on_path: on_path("osascript"),
            notify_send_on_path: on_path("notify-send"),
            display: var("DISPLAY"),
            wayland_display: var("WAYLAND_DISPLAY"),
            ssh_connection: var("SSH_CONNECTION"),
            ssh_tty: var("SSH_TTY"),
        }
    }
}

/// Which desktop tool this mothership can reach, or the short human reason it cannot. Over SSH or on
/// a headless machine there is no desktop to notify — a fact to state once, not a fault to report
/// every tick.
pub fn desktop_tool(env: &DesktopEnv) -> Result<Tool, &'static str> {
    if env.ssh_connection.is_some() || env.ssh_tty.is_some() {
        return Err("the mothership runs over SSH, so there is no desktop to notify");
    }
    match env.os {
        "macos" if env.osascript_on_path => Ok(Tool::Osascript),
        "macos" => Err("osascript is not on the PATH"),
        "linux" if env.display.is_none() && env.wayland_display.is_none() => {
            Err("no graphical session to notify (no DISPLAY or WAYLAND_DISPLAY)")
        }
        "linux" if env.notify_send_on_path => Ok(Tool::NotifySend),
        "linux" => Err("notify-send is not on the PATH"),
        _ => Err("no desktop notification tool for this platform"),
    }
}

/// Interpolates `text` into AppleScript source. osascript takes the notification text as part of the
/// script, not as a separate argument, so a stray quote would be script — escape what AppleScript
/// reads, and drop the control characters and newlines a one-line popup has no use for.
pub fn applescript_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if !c.is_control() => out.push(c),
            _ => {}
        }
    }
    out
}

/// How long a desktop notification may take. Announce runs one event at a time, so a notifier that
/// has stopped answering (a wedged notification daemon) must be cut off, not waited on.
const DESKTOP_TIMEOUT: Duration = Duration::from_secs(10);

/// The desktop half of a notification: the short human reason it did not happen, or nothing. The
/// text travels as an argument, never through a shell; for osascript it lands inside the script,
/// which is what [`applescript_string`] is for.
async fn notify_desktop(tool: Tool, text: &str) -> Option<String> {
    let mut command = match tool {
        Tool::Osascript => {
            let script = format!(
                "display notification \"{}\" with title \"Colonizer\"",
                applescript_string(text)
            );
            let mut command = tokio::process::Command::new("osascript");
            command.arg("-e").arg(script);
            command
        }
        Tool::NotifySend => {
            // `--` ends the flags: a repository's name opens the text, and one beginning with `-`
            // must reach the summary position, never be read as an option.
            let mut command = tokio::process::Command::new("notify-send");
            command.arg("--").arg("Colonizer").arg(text);
            command
        }
    };
    // kill_on_drop so the timeout below cuts a hung notifier off, as util::exec does.
    command.stdin(Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(DESKTOP_TIMEOUT, command.output()).await {
        Err(_) => Some(format!("timed out after {}s", DESKTOP_TIMEOUT.as_secs())),
        Ok(Err(e)) => Some(format!("{e}")),
        Ok(Ok(out)) if !out.status.success() => Some(format!("{}", out.status)),
        Ok(Ok(_)) => None,
    }
}

// ---------------------------------------------------------------------------
// The webhook channel
// ---------------------------------------------------------------------------

/// The whole of what a webhook receives. No issue title, no question text, no branch, no error, no
/// diff: repository content can carry instructions, so nothing of it is sent to an address outside
/// this machine.
pub fn payload(event: Event, at: DateTime<Utc>, session: &Session) -> Value {
    json!({
        "event": event.name(),
        "at": at.to_rfc3339(),
        "text": event.text(&session.repo, session.issue),
        "colony": {
            "id": session.id.clone(),
            "repo": session.repo.clone(),
            "org": session.org.clone(),
            "issue": session.issue,
            "status": session.status,
        },
        // The pull request address only on the event that is about one; the field stays present so
        // a receiver reads one shape.
        "pr_url": if event == Event::PullRequest { session.pr_url.clone() } else { None },
    })
}

/// Whether a webhook URL is one we will POST to. Empty means off; anything set must be http(s),
/// because the client posts nowhere else and a typo should be one clear line in the log.
fn webhook_valid(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// The webhook signature: HMAC-SHA256 over the exact bytes `"{timestamp}.{body}"`, lowercase hex,
/// sent as `sha256=<hex>` next to the timestamp that pinned it. A receiver re-computes it from the
/// raw bytes and refuses the body when neither matches.
pub fn signature(secret: &str, timestamp: &str, body: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let mut message = String::with_capacity(timestamp.len() + 1 + body.len());
    message.push_str(timestamp);
    message.push('.');
    message.push_str(body);
    hex(hmac::sign(&key, message.as_bytes()).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// POSTs one signed body. No retries: the next event tries again, and a webhook that answers 500 is
/// the receiver's problem to describe, not ours to fix.
async fn post(client: &reqwest::Client, url: &str, secret: Option<&str>, body: &str) -> Result<()> {
    let timestamp = Utc::now().timestamp().to_string();
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("X-Colonizer-Timestamp", &timestamp)
        .body(body.to_string());
    if let Some(secret) = secret {
        request = request.header(
            "X-Colonizer-Signature",
            format!("sha256={}", signature(secret, &timestamp, body)),
        );
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        bail!("the webhook answered {status}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The signing secret, stored like the mem0 key
// ---------------------------------------------------------------------------

fn secret_file(app: &App) -> PathBuf {
    app.cfg.config_dir.join("notify-secret")
}

/// The saved webhook signing secret, else `COLONIZER_NOTIFY_SECRET`. Lives beside the other keys
/// and, like them, is never written to modules.json, never returned by the API and never sent into
/// a colony.
fn secret(app: &App) -> Option<(String, &'static str)> {
    read_trimmed(&secret_file(app))
        .map(|key| (key, "file"))
        .or_else(|| env_nonempty("COLONIZER_NOTIFY_SECRET").map(|key| (key, "env")))
}

/// Whether a webhook signing secret is set, and where from. Never the secret.
pub async fn secret_status(State(app): State<Shared>) -> Json<Value> {
    let source = secret(&app).map(|(_, source)| source);
    Json(json!({"has_secret": source.is_some(), "source": source}))
}

#[derive(Deserialize)]
pub struct NotifySecret {
    secret: Option<String>,
}

/// Saves the webhook signing secret on this machine, or removes it when `null`.
pub async fn put_secret(State(app): State<Shared>, Json(req): Json<NotifySecret>) -> ApiResult<Value> {
    let path = secret_file(&app);
    match req.secret.as_deref().map(str::trim) {
        None | Some("") => {
            let _ = std::fs::remove_file(&path);
        }
        Some(value) if value.len() > 512 || !value.chars().all(|c| c.is_ascii_graphic()) => {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                "that doesn't look like a signing secret",
            ));
        }
        Some(value) => write_secret(&path, value)?,
    }
    Ok(secret_status(State(app)).await)
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// How often the session list is diffed. Half the watchdog's minute, so a notification arrives with
/// the flag it reports rather than a minute behind it.
const TICK: Duration = Duration::from_secs(30);

/// Runs the notify module forever: every thirty seconds, diff the session list and announce the
/// edges. When the module is off (or was never configured) the loop clears its bookkeeping and
/// returns, so switching it on later announces only what happens from then on.
pub async fn run(app: Shared) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok();
    if client.is_none() {
        eprintln!("notify: could not build an HTTP client; the webhook channel is off");
    }
    let mut seen: HashMap<String, Seen> = HashMap::new();
    // Reasons already reported, so a persistent problem is one log line, not one a tick.
    let mut desktop_reason: Option<&'static str> = None;
    let mut webhook_reason: Option<String> = None;
    loop {
        tick.tick().await;
        let modules = app.modules.read().await.clone();
        if !modules.notify.as_ref().is_some_and(|c| c.enabled) {
            seen.clear();
            continue;
        }
        let sessions = app.sessions.read().await.clone();
        let mut per_org: HashMap<String, NotifySettings> = HashMap::new();
        let (events, next) = diff(&sessions, &seen, |s| {
            per_org
                .entry(s.org.clone())
                .or_insert_with(|| effective_notify(&modules, &app.org_settings(&s.org)))
                .clone()
        });
        seen = next;
        for (session, event) in events {
            let Some(settings) = per_org.get(&session.org) else { continue };
            announce(
                &app,
                client.as_ref(),
                session,
                event,
                settings,
                &mut desktop_reason,
                &mut webhook_reason,
            )
            .await;
        }
    }
}

/// Announces one event down whichever channels are on. Nothing here is fatal: a channel that cannot
/// be reached is a line in the log, and the next event tries again.
async fn announce(
    app: &App,
    client: Option<&reqwest::Client>,
    session: &Session,
    event: Event,
    settings: &NotifySettings,
    desktop_reason: &mut Option<&'static str>,
    webhook_reason: &mut Option<String>,
) {
    let text = event.text(&session.repo, session.issue);
    if settings.desktop {
        match desktop_tool(&DesktopEnv::this_host()) {
            Ok(tool) => {
                *desktop_reason = None;
                if let Some(what) = notify_desktop(tool, &text).await {
                    // About this colony's event, so it belongs in this colony's log, where the
                    // person it was meant for is looking — as the watchdog and autonomy log.
                    app.session_log(
                        &session.id,
                        "warn",
                        format!("notify: the desktop notification failed ({what})"),
                    )
                    .await;
                }
            }
            Err(reason) if *desktop_reason != Some(reason) => {
                eprintln!("notify: desktop notifications stay off: {reason}");
                *desktop_reason = Some(reason);
            }
            Err(_) => {}
        }
    }
    let Some(client) = client else { return };
    if settings.webhook_url.is_empty() {
        return;
    }
    if !webhook_valid(&settings.webhook_url) {
        if webhook_reason.as_deref() != Some(settings.webhook_url.as_str()) {
            eprintln!(
                "notify: the webhook stays off: {} is not an http:// or https:// address",
                settings.webhook_url
            );
            *webhook_reason = Some(settings.webhook_url.clone());
        }
        return;
    }
    *webhook_reason = None;
    let Ok(body) = serde_json::to_string(&payload(event, Utc::now(), session)) else {
        return;
    };
    // Read where it is used, so saving or removing the secret takes effect without a restart.
    let signing = secret(app);
    if let Err(e) = post(
        client,
        &settings.webhook_url,
        signing.as_ref().map(|(value, _)| value.as_str()),
        &body,
    )
    .await
    {
        app.session_log(&session.id, "warn", format!("notify: the webhook failed ({e:#})"))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};

    fn settings() -> NotifySettings {
        NotifySettings {
            enabled: true,
            on_question: true,
            on_attention: true,
            on_failed: true,
            on_pull_request: true,
            desktop: false,
            webhook_url: String::new(),
        }
    }

    fn seen(status: SessionStatus, attention: Option<&str>) -> Seen {
        Seen {
            status,
            attention: attention.map(String::from),
        }
    }

    /// A colony with nothing worth announcing, as the loop would see it between ticks.
    fn colony(id: &str, status: SessionStatus) -> Session {
        serde_json::from_value(json!({
            "id": id,
            "repo": "acme/webshop",
            "org": "acme",
            "issue": 42,
            "issue_title": "SENTINEL-issue-title",
            "status": status,
            "branch": "colonizer/SENTINEL-branch",
            "worktree": "/colonizer/worktrees/wt",
            "sandbox": "colonizer-abc123",
            "agent": "claude",
            "pr_url": "https://github.com/acme/webshop/pull/7",
            "error": "SENTINEL-error",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    fn fired(settings: &NotifySettings, was: SessionStatus, is: SessionStatus) -> Vec<Event> {
        decide(settings, Some(&seen(was, None)), &seen(is, None))
    }

    #[test]
    fn each_event_fires_once_on_its_edge_and_not_while_it_holds() {
        for (event, status) in [
            (Event::Question, SessionStatus::WaitingForAnswer),
            (Event::Failed, SessionStatus::Failed),
            (Event::PullRequest, SessionStatus::PrOpened),
        ] {
            let s = settings();
            assert_eq!(fired(&s, SessionStatus::Running, status), vec![event]);
            assert!(fired(&s, status, status).is_empty(), "held {:?} is not an edge", status);
            assert!(
                fired(&s, status, SessionStatus::Running).is_empty(),
                "leaving {:?} announces nothing",
                status
            );
            // Leaving and coming back is a new edge, and it fires once again.
            assert!(fired(&s, status, SessionStatus::Running).is_empty());
            assert_eq!(fired(&s, SessionStatus::Running, status), vec![event]);
        }
    }

    #[test]
    fn attention_fires_for_the_watchdogs_reasons_only() {
        let s = settings();
        let edge = |was: Option<&str>, now: Option<&str>| {
            decide(
                &s,
                Some(&seen(SessionStatus::Running, was)),
                &seen(SessionStatus::Running, now),
            )
        };
        assert_eq!(edge(None, Some("stalled")), vec![Event::Attention("stalled")]);
        assert_eq!(
            edge(Some("stalled"), Some("nudges_exhausted")),
            vec![Event::Attention("nudges_exhausted")],
            "out of nudges is a second edge after the stall"
        );
        // Held states are not edges.
        assert!(edge(Some("stalled"), Some("stalled")).is_empty());
        assert!(
            edge(None, Some("waiting_for_answer")).is_empty(),
            "the watchdog's waiting flag is this module's question event, not an attention one"
        );
        assert!(
            edge(None, Some("autopilot_held")).is_empty(),
            "autopilot's flag is not the watchdog's"
        );
        assert!(edge(Some("autopilot_held"), None).is_empty(), "clearing announces nothing");
    }

    #[test]
    fn the_event_switches_decide_what_fires() {
        let mut s = settings();
        s.on_question = false;
        assert!(fired(&s, SessionStatus::Running, SessionStatus::WaitingForAnswer).is_empty());
        assert_eq!(fired(&s, SessionStatus::Running, SessionStatus::Failed), vec![Event::Failed]);
        s = settings();
        s.on_pull_request = false;
        assert!(fired(&s, SessionStatus::Running, SessionStatus::PrOpened).is_empty());
    }

    #[test]
    fn disabled_notify_decides_nothing_and_first_sight_only_seeds() {
        let mut s = settings();
        s.enabled = false;
        assert!(decide(&s, None, &seen(SessionStatus::Failed, Some("stalled"))).is_empty());
        assert!(
            decide(
                &s,
                Some(&seen(SessionStatus::Running, None)),
                &seen(SessionStatus::Failed, None)
            )
            .is_empty()
        );
        assert!(
            decide(&settings(), None, &seen(SessionStatus::Failed, Some("stalled"))).is_empty(),
            "a colony seen for the first time seeds the state instead of announcing a backlog"
        );
    }

    #[test]
    fn the_text_names_the_repo_and_issue_without_repository_content() {
        assert_eq!(
            Event::Question.text("acme/webshop", Some(42)),
            "acme/webshop #42 needs an answer"
        );
        assert_eq!(
            Event::Attention("stalled").text("acme/webshop", Some(42)),
            "acme/webshop #42 has stalled"
        );
        assert_eq!(
            Event::Attention("nudges_exhausted").text("acme/webshop", Some(42)),
            "acme/webshop #42 is out of nudges"
        );
        assert_eq!(Event::Failed.text("acme/webshop", None), "acme/webshop failed");
        assert_eq!(
            Event::PullRequest.text("acme/webshop", None),
            "acme/webshop opened a pull request",
            "a colony with no issue is just the repository"
        );
        let long: String = "r".repeat(MAX_TEXT + 50);
        assert_eq!(
            Event::Failed.text(&long, None).chars().count(),
            MAX_TEXT + 1,
            "capped, ellipsis included"
        );
    }

    #[test]
    fn the_desktop_decision_answers_from_its_inputs_not_the_machine() {
        let linux = DesktopEnv {
            os: "linux",
            notify_send_on_path: true,
            display: Some(":0".into()),
            ..Default::default()
        };
        assert_eq!(desktop_tool(&linux), Ok(Tool::NotifySend));
        let wayland = DesktopEnv {
            os: "linux",
            notify_send_on_path: true,
            wayland_display: Some("wayland-0".into()),
            ..Default::default()
        };
        assert_eq!(desktop_tool(&wayland), Ok(Tool::NotifySend));
        let headless = DesktopEnv {
            os: "linux",
            notify_send_on_path: true,
            ..Default::default()
        };
        assert!(
            desktop_tool(&headless).is_err(),
            "no DISPLAY and no WAYLAND_DISPLAY is no desktop"
        );
        let macos = DesktopEnv {
            os: "macos",
            osascript_on_path: true,
            ..Default::default()
        };
        assert_eq!(desktop_tool(&macos), Ok(Tool::Osascript));
        for mut over_ssh in [linux.clone(), macos.clone()] {
            over_ssh.ssh_connection = Some("203.0.113.7 5222 192.168.0.2 22".into());
            assert!(desktop_tool(&over_ssh).is_err(), "over SSH there is no desktop to notify");
        }
        let no_binary = DesktopEnv {
            os: "linux",
            display: Some(":0".into()),
            ..Default::default()
        };
        assert!(desktop_tool(&no_binary).is_err(), "the tool has to be on the PATH");
        let no_mac_binary = DesktopEnv {
            os: "macos",
            ..Default::default()
        };
        assert!(desktop_tool(&no_mac_binary).is_err());
        assert!(
            desktop_tool(&DesktopEnv {
                os: "windows",
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn applescript_escaping_survives_quotes_and_drops_control_characters() {
        assert_eq!(applescript_string("plain"), "plain");
        assert_eq!(applescript_string(r#"he said "hi""#), r#"he said \"hi\""#);
        assert_eq!(applescript_string("back\\slash"), "back\\\\slash");
        assert_eq!(applescript_string("two\nlines\r\there"), "twolineshere");
        assert_eq!(applescript_string("nul\u{0}byte"), "nulbyte");
    }

    #[test]
    fn the_webhook_payload_carries_no_repository_content() {
        let at = DateTime::from_timestamp(1_789_000_000, 0).unwrap();
        for (event, status) in [
            (Event::Question, SessionStatus::WaitingForAnswer),
            (Event::Attention("stalled"), SessionStatus::Running),
            (Event::Failed, SessionStatus::Failed),
            (Event::PullRequest, SessionStatus::PrOpened),
        ] {
            let session = colony("abc123", status);
            let body = serde_json::to_string(&payload(event, at, &session)).unwrap();
            for sentinel in ["SENTINEL-issue-title", "SENTINEL-branch", "SENTINEL-error"] {
                assert!(!body.contains(sentinel), "{event:?} leaked {sentinel}: {body}");
            }
            let value: Value = serde_json::from_str(&body).unwrap();
            let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                ["at", "colony", "event", "pr_url", "text"],
                "the payload is exactly five keys"
            );
            let mut colony_keys: Vec<&str> = value["colony"].as_object().unwrap().keys().map(String::as_str).collect();
            colony_keys.sort_unstable();
            assert_eq!(colony_keys, ["id", "issue", "org", "repo", "status"]);
            assert_eq!(value["event"], json!(event.name()));
            assert_eq!(value["colony"]["status"], json!(status));
            if event == Event::PullRequest {
                assert_eq!(
                    value["pr_url"],
                    json!("https://github.com/acme/webshop/pull/7"),
                    "the pull request event carries its address"
                );
            } else {
                assert!(value["pr_url"].is_null(), "{event:?} carries no pr_url: {body}");
            }
        }
    }

    #[test]
    fn the_webhook_signature_is_pinned() {
        // Computed once with an independent implementation. Pins the whole scheme: HMAC-SHA256 over
        // `"{timestamp}.{body}"`, lowercase hex, wrapped as `sha256=<hex>` by the sender.
        assert_eq!(
            signature("a-signing-secret-for-tests", "1789000000", r#"{"event":"failed"}"#),
            "4f3fc4526050244f4333184258c3b34374cfa8f9ede75b3e94c321fce6d76712"
        );
        // A different timestamp or body signs differently, so both really are covered by the MAC.
        assert_ne!(
            signature("a-signing-secret-for-tests", "1789000001", r#"{"event":"failed"}"#),
            "4f3fc4526050244f4333184258c3b34374cfa8f9ede75b3e94c321fce6d76712"
        );
    }

    #[test]
    fn a_webhook_url_must_be_http_or_https_and_empty_means_off() {
        assert!(webhook_valid("https://example.com/hook"));
        assert!(webhook_valid("http://127.0.0.1:9000/hook"));
        assert!(!webhook_valid(""), "empty is off, which is not the same as invalid");
        assert!(!webhook_valid("file:///etc/passwd"));
        assert!(!webhook_valid("ftp://example.com"));
        assert!(!webhook_valid("example.com/hook"));
    }

    #[test]
    fn diff_seeds_new_colonies_announces_edges_and_prunes_gone_ones() {
        let s = settings();
        let known = colony("abc123", SessionStatus::Failed);
        let fresh = colony("def456", SessionStatus::Queued);
        let mut seen = HashMap::new();
        seen.insert(
            known.id.clone(),
            Seen {
                status: SessionStatus::Running,
                attention: None,
            },
        );
        // The known colony failed: an edge. The fresh one is seen for the first time: not.
        let list = [known.clone(), fresh.clone()];
        let (events, next) = diff(&list, &seen, |_| s.clone());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0.id, known.id);
        assert_eq!(events[0].1, Event::Failed);
        assert_eq!(next.len(), 2, "the fresh colony is seeded, not announced");
        assert!(next.contains_key(&fresh.id));

        // A colony no longer in the list leaves the map, so it cannot grow forever.
        let remainder = [fresh];
        let (events, next) = diff(&remainder, &next, |_| s.clone());
        assert!(events.is_empty(), "a held state is not an edge");
        assert_eq!(next.len(), 1);
        assert!(!next.contains_key(&known.id));
    }

    #[test]
    fn the_notify_module_schema_defaults_to_every_event_and_no_channel() {
        let schema = crate::modules::providers("notify", &[]).remove(0).schema;
        for key in ["on_question", "on_attention", "on_failed", "on_pull_request"] {
            assert_eq!(schema["properties"][key]["default"], json!(true), "{key} is on by default");
        }
        assert_eq!(schema["properties"]["desktop"]["default"], json!(false));
        assert_eq!(schema["properties"]["webhook_url"]["default"], json!(""));
        assert!(
            schema["properties"]["webhook_url"]["description"]
                .as_str()
                .is_some_and(|d| d.contains("no repository content")),
            "the description says what the webhook carries"
        );
    }

    #[test]
    fn notify_settings_survive_a_round_trip_through_the_module_choice() {
        let mut choice = crate::config::ModuleChoice {
            provider: "default".into(),
            enabled: true,
            settings: Map::new(),
        };
        choice.settings.insert("desktop".into(), json!(true));
        let schema = crate::modules::schema_for("notify", "default", &[]);
        let flag = |key: &str| {
            crate::config::setting(&choice, &schema, key)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        assert!(flag("desktop"), "an explicit setting wins");
        assert!(flag("on_failed"), "a missing setting falls back to the schema default");
    }
}
