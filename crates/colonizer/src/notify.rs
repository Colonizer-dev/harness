//! The notify module: when a colony needs an answer, stalls, fails or opens a pull request, or a
//! model provider starts failing, say so on the desktop the mothership runs on, if a URL is set, at
//! a webhook, and at every phone or desktop subscribed to Web Push.
//!
//! Two choices shape it. Events are detected by diffing the session list — and the providers' usage
//! tallies — in a poll loop, never by hooking the call sites — a notification must not be able to
//! change what a colony does, and a new status change elsewhere cannot forget to tell it. And what
//! leaves the mothership is one short line about the colony, never repository content: a webhook is
//! a write to somewhere outside this machine, and repository content can carry instructions.

use crate::{
    ApiResult, App, Shared, client_error,
    gateway::{ProviderUsage, UsageHealth, health},
    ledger,
    orgs::{OrgSettings, effective_notify},
    protocol::Origin,
    providers::Provider,
    push,
    sessions::{Session, SessionStatus},
    util::{delete_secret, env_nonempty, read_secret, truncate, write_secret},
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
    /// Whether a model provider crossing its failure threshold announces. Providers are not
    /// org-scoped, so no org overrides this.
    pub on_provider: bool,
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
    /// A configured model provider's failure rate crossed the degraded line
    /// ([`crate::gateway::DEGRADED_PCT`]). The one event with no colony behind it.
    ProviderDegraded,
    /// `rebase_orphaned` became true (issue #453): the colony behind a pull request that fell
    /// behind its base is gone, so nothing is left running that will ever rebase it or clear the
    /// flag itself — a person has to.
    NeedsRebase,
}

impl Event {
    /// The event's name in the webhook payload.
    pub fn name(self) -> &'static str {
        match self {
            Event::Question => "question",
            Event::Attention(_) => "attention",
            Event::Failed => "failed",
            Event::PullRequest => "pull_request",
            Event::ProviderDegraded => "provider_degraded",
            Event::NeedsRebase => "needs_rebase",
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
            Event::NeedsRebase => "fell behind its base, but the colony behind it is gone",
            // A provider names no colony, so this session-shaped path is never called with the
            // provider event; its line is [`Event::provider_text`]'s to build.
            Event::ProviderDegraded => {
                unreachable!("provider events have no colony; build their line with Event::provider_text")
            }
        };
        let subject = match issue {
            Some(number) => format!("{repo} #{number}"),
            None => repo.to_string(),
        };
        truncate(&format!("{subject} {what}"), MAX_TEXT)
    }

    /// The one line for [`Event::ProviderDegraded`]: the provider, its failure rate, and — when one
    /// has been seen — the code of its most recent failure. It carries no repository content — the
    /// provider event carries no repository at all, only the id and name the operator chose and the
    /// counters the gateway tallied.
    pub fn provider_text(name: &str, failure_pct: f64, last_failure: Option<&str>) -> String {
        let failure = last_failure.map(|code| format!("; last failure {code}")).unwrap_or_default();
        truncate(
            &format!("{name} is failing {failure_pct:.1}% of its requests{failure}"),
            MAX_TEXT,
        )
    }
}

/// What was last seen of a colony: the whole of what edge detection needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub status: SessionStatus,
    pub attention: Option<String>,
    /// Mirrors [`Session::rebase_orphaned`]: set once nothing is left running to clear
    /// `needs_rebase` on its own.
    pub rebase_orphaned: bool,
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
            rebase_orphaned: session.rebase_orphaned,
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
    // Same switch as attention (issue #453): an orphaned rebase is exactly the kind of thing
    // attention notifications already exist for — something the loop cannot fix itself — and it
    // did not seem worth a dedicated schema field for one more edge in the same family.
    if settings.on_attention && now.rebase_orphaned && !last.rebase_orphaned {
        events.push(Event::NeedsRebase);
    }
    events
}

/// Below this percentage a degraded provider is treated as recovered and the announcement re-arms.
/// Deliberately under [`crate::gateway::DEGRADED_PCT`]: a provider's counters are cumulative for the
/// life of the install, so its rate moves slowly, and one that hovers at the line would otherwise
/// announce on every tick. That same cumulative tally makes re-arming expensive, and the cost should
/// be said plainly: the lifetime rate only falls under this line once healthy traffic has diluted the
/// outage many times over. After issue #184's episode (9,599 failures in 32,689 requests, 29.4%), a
/// provider that never failed again would need roughly 120,000 cumulative requests to get here — so
/// a tally that has lived long enough may in practice never re-arm, and a second, separate outage
/// months later announces nothing.
pub const RECOVER_PCT: f64 = 8.0;

/// Whether a provider's health calls for the one [`Event::ProviderDegraded`] announcement, and the
/// degraded state to keep. Pure, like [`decide`]. The hysteresis is the point: a crossing announces
/// once, holding announces nothing, and re-arming waits for the rate to fall clearly back under the
/// threshold, so a rate sitting between the two lines never flaps — though on a cumulative tally
/// re-arming may in practice never come at all ([`RECOVER_PCT`]). A provider seen for the first time
/// only seeds (`None`), the same convention [`decide`] follows for colonies: a restart must not
/// announce every provider that was already failing before it, as if it had just broken. With the
/// module off, or [`NotifySettings::on_provider`] off, nothing announces and the state is carried
/// through untouched: a provider whose announcement was already spent stays spent, but one that
/// crossed the line while the event was off is carried as not-degraded, so switching the event back
/// on announces it as a fresh crossing. An unrated provider ([`UsageHealth::rated`]) is never
/// degraded.
pub fn decide_provider(
    settings: &NotifySettings,
    was_degraded: Option<bool>,
    health: &UsageHealth,
) -> (bool /* announce */, bool /* now degraded */) {
    if !settings.enabled || !settings.on_provider {
        return (false, was_degraded.unwrap_or(false));
    }
    if !health.rated {
        return (false, false);
    }
    let Some(was) = was_degraded else {
        return (false, health.degraded);
    };
    match (was, health.degraded) {
        (false, true) => (true, true),
        // Under the degraded line but not yet clearly under [`RECOVER_PCT`]: the announcement stays
        // spent. Clearly under it re-arms, so the next crossing announces again.
        (true, false) => (false, health.failure_pct >= RECOVER_PCT),
        // Holding, on either side of the line, is not an edge.
        (true, true) | (false, false) => (false, health.degraded),
    }
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

/// One degraded-provider announcement, carrying what both channels need: the provider and the usage
/// that crossed the line.
#[derive(Clone, Debug)]
pub struct ProviderEvent {
    pub provider: Provider,
    pub usage: ProviderUsage,
    pub health: UsageHealth,
}

/// Diffs the configured providers against what was last announced: the ones to announce, and the
/// degraded state to keep. Entries for providers that no longer exist are simply not carried over,
/// so the map cannot grow forever — the same rule the session diff follows.
fn diff_providers(
    providers: &[Provider],
    degraded: &HashMap<String, bool>,
    settings: &NotifySettings,
    usage_of: impl Fn(&Provider) -> ProviderUsage,
) -> (Vec<ProviderEvent>, HashMap<String, bool>) {
    let mut next = HashMap::with_capacity(providers.len());
    let mut events = Vec::new();
    for provider in providers {
        let usage = usage_of(provider);
        let health = health(&usage);
        let (announce, now) = decide_provider(settings, degraded.get(&provider.id).copied(), &health);
        if announce {
            events.push(ProviderEvent {
                provider: provider.clone(),
                usage,
                health,
            });
        }
        next.insert(provider.id.clone(), now);
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
        // The pull request address only on events that are about one; the field stays present so
        // a receiver reads one shape.
        "pr_url": if matches!(event, Event::PullRequest | Event::NeedsRebase) { session.pr_url.clone() } else { None },
        // Session events are never about a provider; the key stays present so a receiver reads one
        // shape, the same reason `pr_url` is always here.
        "provider": None::<Value>,
    })
}

/// The webhook payload for [`Event::ProviderDegraded`]. One shape with [`payload`]: `colony` and
/// `pr_url` are `null` here, and `provider` carries the id and name the operator chose plus the
/// counters behind the announcement — still nothing of any repository.
pub fn provider_payload(id: &str, name: &str, requests: u64, health: &UsageHealth, at: DateTime<Utc>) -> Value {
    json!({
        "event": Event::ProviderDegraded.name(),
        "at": at.to_rfc3339(),
        "text": Event::provider_text(name, health.failure_pct, health.last_failure.as_deref()),
        "colony": None::<Value>,
        "pr_url": None::<Value>,
        "provider": {
            "id": id,
            "name": name,
            "failure_pct": health.failure_pct,
            "avg_latency_ms": health.avg_latency_ms,
            "requests": requests,
            "failure": health.last_failure,
        },
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
    read_secret(&secret_file(app))
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
            delete_secret(&path);
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

/// Runs the notify module forever: every thirty seconds, diff the session list and the providers'
/// usage tallies against what was last seen, and announce the edges. When the module is off (or was
/// never configured) the loop clears its bookkeeping and returns, so switching it on later announces
/// only what happens from then on.
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
    // Per provider: whether its failure rate has already been announced as degraded. A restart starts
    // empty, so providers that were already failing seed instead of announcing a backlog.
    let mut degraded: HashMap<String, bool> = HashMap::new();
    let mut reasons = Reasons::default();
    loop {
        tick.tick().await;
        let modules = app.modules.read().await.clone();
        if !modules.notify.as_ref().is_some_and(|c| c.enabled) {
            seen.clear();
            degraded.clear();
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
            announce(&app, client.as_ref(), session, event, settings, &mut reasons).await;
        }
        // A provider is not org-scoped — no colony, no org to resolve — so its settings are the
        // notify module's own global choice. `effective_notify` with a default org is exactly that:
        // the module's settings, with the schema defaults for anything it leaves unset, org
        // overrides absent.
        let settings = effective_notify(&modules, &OrgSettings::default());
        let (provider_events, next_degraded) =
            diff_providers(&app.providers(), &degraded, &settings, |p| app.gateway.usage(&p.id));
        degraded = next_degraded;
        for event in &provider_events {
            announce_provider(&app, client.as_ref(), event, &settings, &mut reasons).await;
        }
        // The digest (issue #311): what the soft layers held, one line an hour at most, no identities,
        // down the same channels as any announcement — and the counts it carried are subtracted only
        // once a channel actually took it, so candidates held while it was in flight stay due.
        if let Some((summary, held)) = app.ledger.digest_due(Utc::now()) {
            let at = Utc::now();
            let payload = json!({
                "event": "digest",
                "at": at.to_rfc3339(),
                "text": summary,
                // The same six-key shape every webhook payload carries, with nothing to name here.
                "colony": None::<Value>,
                "pr_url": None::<Value>,
                "provider": None::<Value>,
            });
            if deliver(&app, client.as_ref(), &summary, &payload, None, &settings, &mut reasons).await {
                app.ledger.commit_digest(&held, at).await;
            }
        }
    }
}

/// The channel failures already reported, so a persistent problem is one log line, not one a tick.
#[derive(Default)]
struct Reasons {
    desktop: Option<&'static str>,
    webhook: Option<String>,
}

/// The underlying-fact key a notify event claims, so one observation told once is not told again by
/// another claimant: the reason is part of an attention fact (a stall and an out-of-nudges are two
/// different things), a question fact names the open question, and the pure edge events claim
/// nothing — the edge detector already fires each of them exactly once, and the topic's cooldown
/// still bounds flaps. A key, never text.
fn fact_key(event: Event, session: &str, open_question: Option<&str>) -> Option<String> {
    match event {
        Event::Attention(reason) => Some(format!("attention:{reason}:{session}")),
        Event::Question => open_question.map(|id| format!("question:{session}:{id}")),
        Event::Failed | Event::PullRequest | Event::NeedsRebase | Event::ProviderDegraded => None,
    }
}

/// The colony's open question id, if it still has one — the notify loop reads it only to make a
/// question's fact key precise, and a colony with no runtime to ask claims nothing.
async fn open_question_id(app: &App, session: &str) -> Option<String> {
    let runtime = app.runtimes.lock().await.get(session).cloned()?;
    runtime.open_question.lock().await.as_ref().map(|(id, _, _)| id.clone())
}

/// Announces one colony event down whichever channels are on — first asking the shared anti-spam
/// ledger (issue #311). A question blocks its colony, so it is a priority candidate: the soft layers
/// (quiet hours, cooldown, the hourly quota) give way for it, the hard ones do not.
async fn announce(
    app: &App,
    client: Option<&reqwest::Client>,
    session: &Session,
    event: Event,
    settings: &NotifySettings,
    reasons: &mut Reasons,
) {
    let open_question = open_question_id(app, &session.id).await;
    let candidate = ledger::Candidate {
        kind: ledger::Kind::Notify,
        topic: format!("{}:{}", event.name(), session.id),
        class: event.name().to_string(),
        fact: fact_key(event, &session.id, open_question.as_deref()),
        colony: Some(session.id.clone()),
        priority: matches!(event, Event::Question),
    };
    let verdict = app.ledger.check(&candidate, Utc::now());
    if verdict != ledger::Verdict::Deliver {
        // Held or dropped: counted either way — what was held lands in the hour's digest line, what
        // was dropped stands in the tallies the status poll reports. Never silent.
        app.ledger.record(&candidate, &verdict, Utc::now()).await;
        return;
    }
    let text = event.text(&session.repo, session.issue);
    let payload = payload(event, Utc::now(), session);
    if deliver(app, client, &text, &payload, Some(session), settings, reasons).await {
        app.ledger.record(&candidate, &verdict, Utc::now()).await;
    } else {
        // No channel took it: counted as dropped, spending nobody's quota — a send that reached
        // nothing was not a delivery.
        app.ledger
            .record(&candidate, &ledger::Verdict::Drop("undelivered"), Utc::now())
            .await;
    }
}

/// Announces one provider event down the same channels as [`announce`], against the same ledger. A
/// provider has no colony, so there is no colony log to record a failed channel in and the line goes
/// to stderr instead.
async fn announce_provider(
    app: &App,
    client: Option<&reqwest::Client>,
    event: &ProviderEvent,
    settings: &NotifySettings,
    reasons: &mut Reasons,
) {
    let candidate = ledger::Candidate {
        kind: ledger::Kind::Notify,
        topic: format!("provider:{}", event.provider.id),
        class: Event::ProviderDegraded.name().to_string(),
        // Nothing to claim: the crossing edge fires once, and the cooldown holds a re-crossing
        // inside ten minutes instead of losing it.
        fact: None,
        colony: None,
        priority: false,
    };
    let verdict = app.ledger.check(&candidate, Utc::now());
    if verdict != ledger::Verdict::Deliver {
        app.ledger.record(&candidate, &verdict, Utc::now()).await;
        return;
    }
    let name = &event.provider.name;
    let text = Event::provider_text(name, event.health.failure_pct, event.health.last_failure.as_deref());
    let payload = provider_payload(&event.provider.id, name, event.usage.requests, &event.health, Utc::now());
    if deliver(app, client, &text, &payload, None, settings, reasons).await {
        app.ledger.record(&candidate, &verdict, Utc::now()).await;
    } else {
        app.ledger
            .record(&candidate, &ledger::Verdict::Drop("undelivered"), Utc::now())
            .await;
    }
}

/// The channels themselves: the desktop popup, the signed webhook POST, and Web Push. Nothing here
/// is fatal: a channel that cannot be reached is a line in a log, and the next event tries again.
/// `session` is the colony the event is about — provider events have none, and their channel
/// failures land on stderr instead of that colony's log. Answers whether anything actually went
/// out — at least one channel that was on succeeded — so the caller's ledger record only spends a
/// quota on a real delivery.
async fn deliver(
    app: &App,
    client: Option<&reqwest::Client>,
    text: &str,
    payload: &Value,
    session: Option<&Session>,
    settings: &NotifySettings,
    reasons: &mut Reasons,
) -> bool {
    let mut sent = false;
    if settings.desktop {
        match desktop_tool(&DesktopEnv::this_host()) {
            Ok(tool) => {
                reasons.desktop = None;
                if let Some(what) = notify_desktop(tool, text).await {
                    report_failure(app, session, format!("notify: the desktop notification failed ({what})")).await;
                } else {
                    sent = true;
                }
            }
            Err(reason) if reasons.desktop != Some(reason) => {
                eprintln!("notify: desktop notifications stay off: {reason}");
                reasons.desktop = Some(reason);
            }
            Err(_) => {}
        }
    }
    let Some(client) = client else { return sent };
    if post_webhook(app, client, payload, session, settings, reasons).await {
        sent = true;
    }
    // Push has no settings of its own: a subscription is the opt-in (issue #516). The payload
    // carries the same event name and the same one line the other channels do.
    if push::deliver(
        app,
        client,
        payload["event"].as_str().unwrap_or("notify"),
        text,
        session.map(|s| s.id.as_str()),
    )
    .await
    {
        sent = true;
    }
    sent
}

/// The webhook channel: one signed POST of the payload, when a URL is set. `false` means nothing
/// went out — no URL, a bad one, or a receiver that answered with an error.
async fn post_webhook(
    app: &App,
    client: &reqwest::Client,
    payload: &Value,
    session: Option<&Session>,
    settings: &NotifySettings,
    reasons: &mut Reasons,
) -> bool {
    if settings.webhook_url.is_empty() {
        return false;
    }
    if !webhook_valid(&settings.webhook_url) {
        if reasons.webhook.as_deref() != Some(settings.webhook_url.as_str()) {
            eprintln!(
                "notify: the webhook stays off: {} is not an http:// or https:// address",
                settings.webhook_url
            );
            reasons.webhook = Some(settings.webhook_url.clone());
        }
        return false;
    }
    reasons.webhook = None;
    let Ok(body) = serde_json::to_string(payload) else {
        return false;
    };
    // Read where it is used, so saving or removing the secret takes effect without a restart.
    let signing = secret(app);
    match post(
        client,
        &settings.webhook_url,
        signing.as_ref().map(|(value, _)| value.as_str()),
        &body,
    )
    .await
    {
        Ok(()) => true,
        Err(e) => {
            report_failure(app, session, format!("notify: the webhook failed ({e:#})")).await;
            false
        }
    }
}

/// Where a failed channel's line goes: into the colony's log when the event is about a colony, so it
/// lands where the person it was meant for is looking — as the watchdog and autonomy log — or onto
/// stderr when it is about a provider, which has no colony to log into.
async fn report_failure(app: &App, session: Option<&Session>, what: String) {
    match session {
        Some(session) => app.session_log_as(Origin::Notify, &session.id, "warn", what).await,
        None => eprintln!("{what}"),
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
            on_provider: true,
            desktop: false,
            webhook_url: String::new(),
        }
    }

    /// A provider as `providers.json` holds one: an id and name the operator chose, plus defaults.
    fn provider(id: &str, name: &str) -> Provider {
        Provider {
            id: id.into(),
            name: name.into(),
            base_url: "https://provider.test/v1".into(),
            auth: "x-api-key".into(),
            wire: Default::default(),
            models: Vec::new(),
            preset: String::new(),
            timeout_secs: None,
            max_concurrent: None,
            queue_timeout_secs: None,
            context_tokens: None,
            fallback_model: None,
            pricing: None,
            model_map: Default::default(),
            disabled_tools: Vec::new(),
            quota: None,
            normalize_cache_ttl: false,
            trusted: false,
        }
    }

    /// Enough requests for the rule to speak: `failures/requests` at the named rate.
    fn usage(requests: u64, failures: u64) -> ProviderUsage {
        ProviderUsage {
            requests,
            failures,
            duration_ms: requests * 100,
            ..Default::default()
        }
    }

    fn seen(status: SessionStatus, attention: Option<&str>) -> Seen {
        Seen {
            status,
            attention: attention.map(String::from),
            rebase_orphaned: false,
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
    fn rebase_orphaned_becoming_true_fires_once_and_only_with_on_attention() {
        let s = settings();
        let orphaned = |rebase_orphaned: bool| Seen {
            status: SessionStatus::PrOpened,
            attention: None,
            rebase_orphaned,
        };
        assert_eq!(
            decide(&s, Some(&orphaned(false)), &orphaned(true)),
            vec![Event::NeedsRebase],
            "becoming orphaned is the edge"
        );
        assert!(
            decide(&s, Some(&orphaned(true)), &orphaned(true)).is_empty(),
            "held orphaned is not an edge"
        );
        assert!(
            decide(&s, Some(&orphaned(true)), &orphaned(false)).is_empty(),
            "clearing announces nothing"
        );
        let mut off = s.clone();
        off.on_attention = false;
        assert!(
            decide(&off, Some(&orphaned(false)), &orphaned(true)).is_empty(),
            "gated by the same switch as attention"
        );
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
            (Event::NeedsRebase, SessionStatus::PrOpened),
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
                ["at", "colony", "event", "pr_url", "provider", "text"],
                "the payload is exactly six keys, one shape for receivers"
            );
            let mut colony_keys: Vec<&str> = value["colony"].as_object().unwrap().keys().map(String::as_str).collect();
            colony_keys.sort_unstable();
            assert_eq!(colony_keys, ["id", "issue", "org", "repo", "status"]);
            assert_eq!(value["event"], json!(event.name()));
            assert_eq!(value["colony"]["status"], json!(status));
            assert!(
                value["provider"].is_null(),
                "a session event is never about a provider: {body}"
            );
            if matches!(event, Event::PullRequest | Event::NeedsRebase) {
                assert_eq!(
                    value["pr_url"],
                    json!("https://github.com/acme/webshop/pull/7"),
                    "events about a pull request carry its address"
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
                rebase_orphaned: false,
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
    fn a_degraded_provider_announces_once_and_rearms_only_below_the_clear_line() {
        let s = settings();
        let degraded = UsageHealth {
            failure_pct: 29.4,
            avg_latency_ms: 1_200,
            rated: true,
            degraded: true,
            last_failure: None,
        };
        let ok = UsageHealth {
            failure_pct: 3.2,
            avg_latency_ms: 900,
            rated: true,
            degraded: false,
            last_failure: None,
        };
        // A provider seen for the first time only seeds, whatever its rate: a restart must not
        // announce every provider that was already failing before it.
        assert_eq!(decide_provider(&s, None, &degraded), (false, true));
        // Holding is not an edge; the crossing announces once.
        assert_eq!(decide_provider(&s, Some(true), &degraded), (false, true));
        assert_eq!(decide_provider(&s, Some(false), &degraded), (true, true));
        assert_eq!(decide_provider(&s, Some(false), &ok), (false, false));
        // Between the lines is not recovered: 9% is under the degraded line but the announcement
        // stays spent, so a rate hovering at the line does not flap.
        let hovering = UsageHealth {
            failure_pct: 9.0,
            avg_latency_ms: 900,
            rated: true,
            degraded: false,
            last_failure: None,
        };
        assert_eq!(decide_provider(&s, Some(true), &hovering), (false, true));
        assert_eq!(decide_provider(&s, Some(false), &hovering), (false, false));
        // Clearly under 8% re-arms, so the next crossing announces again.
        let recovered = UsageHealth {
            failure_pct: 7.9,
            avg_latency_ms: 900,
            rated: true,
            degraded: false,
            last_failure: None,
        };
        assert_eq!(decide_provider(&s, Some(true), &recovered), (false, false));
        assert_eq!(decide_provider(&s, Some(false), &degraded), (true, true));
    }

    #[test]
    fn provider_events_respect_the_switches_and_an_unrated_provider_is_never_degraded() {
        let degraded = UsageHealth {
            failure_pct: 29.4,
            avg_latency_ms: 1_200,
            rated: true,
            degraded: true,
            last_failure: None,
        };
        let mut s = settings();
        s.enabled = false;
        assert!(!decide_provider(&s, Some(false), &degraded).0);
        assert_eq!(
            decide_provider(&s, Some(true), &degraded),
            (false, true),
            "the state is carried through untouched, so re-enabling announces no backlog"
        );
        s = settings();
        s.on_provider = false;
        assert!(!decide_provider(&s, Some(false), &degraded).0);
        // A provider with too few requests to judge is noise, never a degraded one.
        let unrated = UsageHealth {
            failure_pct: 40.0,
            avg_latency_ms: 800,
            rated: false,
            degraded: false,
            last_failure: None,
        };
        for was in [None, Some(false), Some(true)] {
            assert_eq!(decide_provider(&settings(), was, &unrated), (false, false));
        }
    }

    #[test]
    fn diff_providers_seeds_new_ones_announces_crossings_and_prunes_gone_ones() {
        let s = settings();
        let zai = provider("zai", "zai");
        let local = provider("local", "Local model");
        let failing = usage(100, 30);
        let healthy = usage(100, 1);
        let table =
            |zai_usage: ProviderUsage| HashMap::from([("zai".to_string(), zai_usage), ("local".to_string(), healthy.clone())]);
        let read = |table: HashMap<String, ProviderUsage>| move |p: &Provider| table[&p.id].clone();

        // First sight of both: nothing announces, both are seeded with their current state.
        let (events, next) = diff_providers(
            &[zai.clone(), local.clone()],
            &HashMap::new(),
            &s,
            read(table(failing.clone())),
        );
        assert!(events.is_empty(), "first sight only seeds");
        assert_eq!(next, HashMap::from([("zai".to_string(), true), ("local".to_string(), false)]));

        // Holding is not an edge.
        let (events, _) = diff_providers(&[zai.clone(), local.clone()], &next, &s, read(table(failing.clone())));
        assert!(events.is_empty());

        // Clearly recovered re-arms; the next crossing announces once, about the right provider.
        let (_, armed) = diff_providers(&[zai.clone(), local.clone()], &next, &s, read(table(usage(100, 5))));
        assert!(!armed["zai"]);
        let (events, _) = diff_providers(&[zai.clone(), local.clone()], &armed, &s, read(table(failing.clone())));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].provider.id, "zai");
        assert_eq!(events[0].provider.name, "zai");
        assert_eq!(events[0].health.failure_pct, 30.0);
        assert_eq!(events[0].usage.requests, 100);

        // A provider no longer in providers.json leaves the map, so it cannot grow forever.
        let (_, next) = diff_providers(std::slice::from_ref(&local), &armed, &s, read(table(failing.clone())));
        assert!(!next.contains_key("zai"));
    }

    #[test]
    fn the_provider_text_and_payload_carry_no_repository_content() {
        assert_eq!(
            Event::provider_text("zai", 29.4, None),
            "zai is failing 29.4% of its requests"
        );
        assert_eq!(
            Event::provider_text("zai", 29.4, Some("quota_exhausted")),
            "zai is failing 29.4% of its requests; last failure quota_exhausted"
        );
        let long_name: String = "z".repeat(MAX_TEXT + 50);
        assert_eq!(
            Event::provider_text(&long_name, 29.4, None).chars().count(),
            MAX_TEXT + 1,
            "capped, ellipsis included, like the session text"
        );

        let at = DateTime::from_timestamp(1_789_000_000, 0).unwrap();
        let tallies = ProviderUsage {
            requests: 1_000,
            failures: 294,
            duration_ms: 48_000_000,
            ..Default::default()
        };
        let verdict = health(&tallies);
        let body = serde_json::to_string(&provider_payload("zai", "zai", tallies.requests, &verdict, at)).unwrap();
        assert_eq!(
            Event::provider_text("zai", verdict.failure_pct, verdict.last_failure.as_deref()),
            "zai is failing 29.4% of its requests"
        );
        assert!(!body.contains("acme"), "a provider event carries no repository: {body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["at", "colony", "event", "pr_url", "provider", "text"],
            "one shape with the session payload"
        );
        assert!(value["colony"].is_null(), "no colony behind a provider event: {body}");
        assert!(value["pr_url"].is_null());
        assert_eq!(value["event"], "provider_degraded");
        let mut provider_keys: Vec<&str> = value["provider"].as_object().unwrap().keys().map(String::as_str).collect();
        provider_keys.sort_unstable();
        assert_eq!(
            provider_keys,
            ["avg_latency_ms", "failure", "failure_pct", "id", "name", "requests"]
        );
        assert_eq!(
            value["provider"],
            json!({
                "id": "zai", "name": "zai", "failure_pct": 29.4, "avg_latency_ms": 48_000, "requests": 1_000,
                "failure": null
            })
        );

        // A provider the gateway has seen fail names the code it last failed with.
        let mut last = verdict.clone();
        last.last_failure = Some("unreachable".into());
        let payload = provider_payload("zai", "zai", tallies.requests, &last, at);
        assert_eq!(payload["provider"]["failure"], "unreachable");
        assert_eq!(
            payload["text"],
            "zai is failing 29.4% of its requests; last failure unreachable"
        );
    }

    #[test]
    fn the_notify_module_schema_defaults_to_every_event_and_no_channel() {
        let schema = crate::modules::providers("notify", &[]).remove(0).schema;
        for key in ["on_question", "on_attention", "on_failed", "on_pull_request", "on_provider"] {
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

    /// A notify candidate as `announce` builds one, for the fact-key rules.
    fn notify_candidate(event: Event, session: &str, open_question: Option<&str>) -> ledger::Candidate {
        ledger::Candidate {
            kind: ledger::Kind::Notify,
            topic: format!("{}:{session}", event.name()),
            class: event.name().to_string(),
            fact: fact_key(event, session, open_question),
            colony: Some(session.to_string()),
            priority: matches!(event, Event::Question),
        }
    }

    #[test]
    fn a_stall_and_an_out_of_nudges_are_two_facts_so_both_announce_within_the_hour() {
        let limits = ledger::Limits::for_kind(ledger::Kind::Notify);
        let t0 = DateTime::from_timestamp(1_789_000_000, 0).unwrap();
        let stalled = notify_candidate(Event::Attention("stalled"), "abc123", None);
        let nudged = notify_candidate(Event::Attention("nudges_exhausted"), "abc123", None);
        assert_ne!(stalled.fact, nudged.fact, "two reasons are two facts, even for one colony");
        let mut st = ledger::LedgerState::default();
        assert_eq!(ledger::check(&st, &limits, &stalled, t0), ledger::Verdict::Deliver);
        ledger::record(&mut st, &stalled, &ledger::Verdict::Deliver, t0);
        // The class-only key would have dropped this as a duplicate of the stall; past the topic
        // cooldown, the second reason announces too — nothing was consumed by the first edge.
        assert_eq!(
            ledger::check(&st, &limits, &nudged, t0 + chrono::Duration::minutes(15)),
            ledger::Verdict::Deliver
        );
    }

    #[test]
    fn the_fact_key_names_the_question_or_nothing_and_never_the_class_alone() {
        assert_eq!(
            fact_key(Event::Question, "abc123", Some("q7")).as_deref(),
            Some("question:abc123:q7")
        );
        assert_eq!(
            fact_key(Event::Question, "abc123", None),
            None,
            "no question to name claims nothing"
        );
        assert_eq!(
            fact_key(Event::Attention("stalled"), "abc123", None).as_deref(),
            Some("attention:stalled:abc123")
        );
        for event in [Event::Failed, Event::PullRequest, Event::NeedsRebase, Event::ProviderDegraded] {
            assert_eq!(
                fact_key(event, "abc123", Some("q7")),
                None,
                "{event:?} claims nothing: the edge fires once"
            );
        }
    }
}
