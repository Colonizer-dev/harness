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
use std::{collections::BTreeMap, collections::BTreeSet, collections::HashMap, path::PathBuf, process::Stdio, time::Duration};

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
    /// Whether a provider running out of quota with colonies blocked on it announces (issue #767):
    /// one line per provider, never one per colony. A card spans orgs, so no org overrides this.
    pub on_quota: bool,
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
    /// The autonomy judge could not reach its primary model for three judged calls in a row
    /// (autonomy.rs `MAX_TRANSPORT_FAILURES`, issue #875) — the failure that used to be silent.
    /// Like [`Event::ProviderDegraded`], host-level: no colony behind it.
    JudgeDegraded,
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
            Event::JudgeDegraded => "judge_degraded",
            Event::NeedsRebase => "needs_rebase",
        }
    }

    /// The one line both channels carry: the colony, and what happened. No issue title, no question
    /// text, no branch — for the webhook this line is all of what leaves the mothership.
    pub fn text(self, repo: &str, issue: Option<u64>) -> String {
        let what = match self {
            Event::Question => "needs an answer",
            Event::Attention("nudges_exhausted") => "is out of nudges",
            Event::Attention(crate::watchdog::CONTROL_DEFEAT_REASON) => "may have got past one of its controls",
            Event::Attention(crate::queue::HOLD_UNANSWERED_REASON) => "is parked on a question too risky to answer on its own",
            Event::Attention(_) => "has stalled",
            Event::Failed => "failed",
            Event::PullRequest => "opened a pull request",
            Event::NeedsRebase => "fell behind its base, but the colony behind it is gone",
            // A provider names no colony, so this session-shaped path is never called with the
            // provider event; its line is [`Event::provider_text`]'s to build.
            Event::ProviderDegraded => {
                unreachable!("provider events have no colony; build their line with Event::provider_text")
            }
            // Same shape: the judge names no colony either, and its line is
            // [`Event::judge_text`]'s to build.
            Event::JudgeDegraded => {
                unreachable!("judge events have no colony; build their line with Event::judge_text")
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

    /// The one line for [`Event::JudgeDegraded`] (issue #875): what could not be reached and what its
    /// provider said. It carries no repository content — only the provider id the operator chose and
    /// the error the judge already recorded.
    pub fn judge_text(provider: &str, message: &str) -> String {
        truncate(&format!("The autonomy judge can't reach {provider}: {message}"), MAX_TEXT)
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
        for reason in [
            "stalled",
            "nudges_exhausted",
            crate::queue::HOLD_UNANSWERED_REASON,
            crate::watchdog::CONTROL_DEFEAT_REASON,
        ] {
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

/// One Claude-account edge between two notify ticks (issue #984). Host-level like a provider
/// crossing: one account serves many colonies, so no single colony is named.
#[derive(Clone, Debug, PartialEq)]
pub enum AccountEdge {
    /// The account entered trouble — its sign-in expired or was revoked. What a person has to hear
    /// about, once.
    Entered {
        account: String,
        state: crate::account_health::State,
        waiting: usize,
    },
    /// The account works again.
    Cleared { account: String },
}

/// The account-trouble edges between two ticks, and the state to keep. Pure, like [`decide`]: an
/// account entering trouble announces once, a repeat of the same state is silent, and a cleared one
/// announces its resolution.
pub fn account_edges(
    last: &BTreeMap<String, crate::account_health::State>,
    now: &[(String, crate::account_health::Trouble)],
    waiting: impl Fn(&str) -> usize,
) -> (Vec<AccountEdge>, BTreeMap<String, crate::account_health::State>) {
    let mut next = BTreeMap::new();
    let mut edges = Vec::new();
    for (account, trouble) in now {
        if last.get(account) != Some(&trouble.state) {
            edges.push(AccountEdge::Entered {
                account: account.clone(),
                state: trouble.state,
                waiting: waiting(account),
            });
        }
        next.insert(account.clone(), trouble.state);
    }
    for account in last.keys() {
        if !next.contains_key(account) {
            edges.push(AccountEdge::Cleared {
                account: account.clone(),
            });
        }
    }
    (edges, next)
}

/// One provider-out-of-quota card as the notify loop sees it (issue #767): the provider, the name
/// the operator gave it, when its plan resets, and every colony blocked on it. Built by
/// [`crate::quota_cards::notify_cards`] from the same derivation as the Inbox card.
#[derive(Clone, Debug)]
pub struct QuotaCard {
    pub provider: String,
    pub name: String,
    pub reset_at: Option<String>,
    pub colonies: Vec<Session>,
}

/// The one line an out-of-quota card announces: the provider's name, how many colonies it holds and
/// when it resets. Labels only — no colony's question, issue title or output.
pub fn quota_text(name: &str, reset_at: Option<&str>, colonies: usize) -> String {
    let held = if colonies == 1 {
        "1 colony is waiting".to_string()
    } else {
        format!("{colonies} colonies are waiting")
    };
    let reset = reset_at.map(|r| format!("; resets {r}")).unwrap_or_default();
    truncate(&format!("{name} is out of quota: {held}{reset}"), MAX_TEXT)
}

/// The out-of-quota cards to announce this tick, and the providers to remember as announced. Pure,
/// like [`account_edges`]: a card that appears announces once — however many colonies it holds —
/// a card still open is silent, and a card that closed (the plan reset, or every colony moved on)
/// re-arms, so the next exhaustion announces again. With the module or [`NotifySettings::on_quota`]
/// off nothing announces, and only cards already announced stay spent: a card that opened while the
/// event was off announces when it is switched back on, as a provider crossing does.
pub fn quota_edges<'a>(
    settings: &NotifySettings,
    announced: &BTreeSet<String>,
    cards: &'a [QuotaCard],
) -> (Vec<&'a QuotaCard>, BTreeSet<String>) {
    let on = settings.enabled && settings.on_quota;
    let mut next = BTreeSet::new();
    let mut edges = Vec::new();
    for card in cards {
        if announced.contains(&card.provider) {
            next.insert(card.provider.clone());
        } else if on {
            edges.push(card);
            next.insert(card.provider.clone());
        }
    }
    (edges, next)
}

/// The webhook payload for an out-of-quota card: the six-key shape, `colony` and `pr_url` null, and
/// `provider` the id, name, reset time and how many colonies wait — nothing of any repository.
pub fn quota_payload(card: &QuotaCard, at: DateTime<Utc>) -> Value {
    json!({
        "event": crate::push_prefs::QUOTA,
        "at": at.to_rfc3339(),
        "text": quota_text(&card.name, card.reset_at.as_deref(), card.colonies.len()),
        "colony": None::<Value>,
        "pr_url": None::<Value>,
        "provider": {
            "id": card.provider,
            "name": card.name,
            "reset_at": card.reset_at,
            "colonies": card.colonies.len(),
        },
    })
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

/// The webhook payload for [`Event::JudgeDegraded`] (issue #875). One shape with [`payload`]:
/// `colony` and `pr_url` are `null`, and `provider` carries the id and the judge's own error, which
/// is the provider's words or a transport reason — never repository content.
pub fn judge_payload(provider: &str, kind: &str, status: Option<u16>, message: &str, at: DateTime<Utc>) -> Value {
    json!({
        "event": Event::JudgeDegraded.name(),
        "at": at.to_rfc3339(),
        "text": Event::judge_text(provider, message),
        "colony": None::<Value>,
        "pr_url": None::<Value>,
        "provider": {"id": provider, "kind": kind, "status": status, "message": message},
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
    // Whether the judge's outage has already been announced for the current streak. Mirrors
    // `degraded`: a restart seeds from nothing, and the edge is `alerted` turning on.
    let mut judge_alerted = false;
    // Per Claude account: its last-seen trouble state, so an account that entered, changed or left
    // trouble announces once (issue #984).
    let mut account_states: BTreeMap<String, crate::account_health::State> = BTreeMap::new();
    // The providers whose out-of-quota card has been announced (issue #767), so a card is one push
    // however many colonies it holds and however many ticks it stays open.
    let mut quota_announced: BTreeSet<String> = BTreeSet::new();
    let mut reasons = Reasons::default();
    loop {
        tick.tick().await;
        let modules = app.modules.read().await.clone();
        if !modules.notify.as_ref().is_some_and(|c| c.enabled) {
            seen.clear();
            degraded.clear();
            account_states.clear();
            quota_announced.clear();
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
        // The judge's own outage: host-level like a provider event, raised exactly once per streak
        // by the `alerted` edge the autonomy module owns.
        let judge = app.judge_health.lock().await.clone();
        if judge.alerted
            && !judge_alerted
            && let Some(error) = judge.last_error.as_ref()
        {
            announce_judge(&app, client.as_ref(), error, &settings, &mut reasons).await;
        }
        judge_alerted = judge.alerted;
        // Claude-account trouble (issue #984): host-level like a provider crossing, but one account
        // serves many colonies — the edge names the account and how many wait on it, once per state
        // change, never once per colony. The per-colony path stays quiet for `waiting_for_account`
        // (see `decide`), so ten waiting colonies are one line, not ten.
        let accounts_troubled = crate::account_health::snapshot(&app).await;
        let (account_edges, next_account_states) = account_edges(&account_states, &accounts_troubled, |account| {
            crate::account_health::waiting_on(&sessions, account)
        });
        account_states = next_account_states;
        for edge in &account_edges {
            announce_account(&app, client.as_ref(), edge, &settings, &mut reasons).await;
        }
        // Out-of-quota cards (issue #767): one announcement per provider whose card opened, naming
        // the provider and its reset, never the colonies' own words.
        let cards = crate::quota_cards::notify_cards(&app).await;
        let (quota, next_quota) = quota_edges(&settings, &quota_announced, &cards);
        quota_announced = next_quota;
        for card in quota {
            announce_quota(&app, client.as_ref(), card, &settings, &mut reasons).await;
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
        Event::Failed | Event::PullRequest | Event::NeedsRebase | Event::ProviderDegraded | Event::JudgeDegraded => None,
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

/// The shared tail of a host-level announcement — a provider's, the judge's or a Claude account's
/// (issue #984): the ledger's verdict, the delivery, and the record, so a held or dropped candidate
/// is counted, never silent. A host-level event has no colony, so a failed channel goes to stderr.
async fn announce_host(
    app: &App,
    client: Option<&reqwest::Client>,
    candidate: ledger::Candidate,
    text: String,
    payload: Value,
    settings: &NotifySettings,
    reasons: &mut Reasons,
) {
    announce_routed(
        app,
        client,
        candidate,
        text,
        payload,
        settings,
        reasons,
        PushRoute::Event(None),
    )
    .await;
}

/// [`announce_host`] with the push channel's route spelled out: the out-of-quota card (issue #767)
/// pushes once per provider to the devices whose scope holds any of its colonies.
#[allow(clippy::too_many_arguments)]
async fn announce_routed(
    app: &App,
    client: Option<&reqwest::Client>,
    candidate: ledger::Candidate,
    text: String,
    payload: Value,
    settings: &NotifySettings,
    reasons: &mut Reasons,
    route: PushRoute<'_>,
) {
    let verdict = app.ledger.check(&candidate, Utc::now());
    if verdict != ledger::Verdict::Deliver {
        app.ledger.record(&candidate, &verdict, Utc::now()).await;
        return;
    }
    let text = truncate(&text, MAX_TEXT);
    if deliver_routed(app, client, &text, &payload, settings, reasons, route).await {
        app.ledger.record(&candidate, &verdict, Utc::now()).await;
    } else {
        app.ledger
            .record(&candidate, &ledger::Verdict::Drop("undelivered"), Utc::now())
            .await;
    }
}

/// Announces one provider event down the same channels as [`announce`], against the same ledger.
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
    let name = &event.provider.name;
    let text = Event::provider_text(name, event.health.failure_pct, event.health.last_failure.as_deref());
    let payload = provider_payload(&event.provider.id, name, event.usage.requests, &event.health, Utc::now());
    announce_host(app, client, candidate, text, payload, settings, reasons).await;
}

/// Announces the judge's one outage down the same channels, and the same ledger, as
/// [`announce_provider`]. The autonomy module raises `alerted` for exactly one streak and clears it on
/// a primary success, so the caller's edge is the crossing, not a per-tick condition.
async fn announce_judge(
    app: &App,
    client: Option<&reqwest::Client>,
    error: &crate::autonomy::Failure,
    settings: &NotifySettings,
    reasons: &mut Reasons,
) {
    let candidate = ledger::Candidate {
        kind: ledger::Kind::Notify,
        topic: format!("judge:{}", error.provider),
        class: Event::JudgeDegraded.name().to_string(),
        // Nothing to claim: the raising edge fires once, and the cooldown holds a re-crossing
        // inside ten minutes instead of losing it.
        fact: None,
        colony: None,
        priority: false,
    };
    let status = error.status.map(|s| format!("{s} ")).unwrap_or_default();
    let text = Event::judge_text(&error.provider, &format!("{status}{}", error.message));
    let payload = judge_payload(&error.provider, error.kind.as_str(), error.status, &error.message, Utc::now());
    announce_host(app, client, candidate, text, payload, settings, reasons).await;
}

/// Announces one Claude-account edge down the same channels as [`announce_judge`] (issue #984). The
/// fact key `account:<id>:<state>` makes a flap inside the dedup window one fact, so ten colonies
/// failing an account cost the operator a single line. Host-level, so a failed channel goes to stderr.
async fn announce_account(
    app: &App,
    client: Option<&reqwest::Client>,
    edge: &AccountEdge,
    settings: &NotifySettings,
    reasons: &mut Reasons,
) {
    let (account, event, text, fact) = match edge {
        AccountEdge::Entered { account, state, waiting } => (
            account.clone(),
            "account_needs_sign_in",
            crate::account_health::trouble_text(account, *waiting),
            format!("account:{account}:{}", state.as_str()),
        ),
        AccountEdge::Cleared { account } => (
            account.clone(),
            "account_resolved",
            format!("Claude account `{account}` works again; its colonies are resuming."),
            format!("account:{account}:resolved"),
        ),
    };
    let candidate = ledger::Candidate {
        kind: ledger::Kind::Notify,
        topic: format!("account:{account}"),
        class: event.to_string(),
        fact: Some(fact),
        colony: None,
        priority: false,
    };
    let payload = json!({
        "event": event,
        "at": Utc::now().to_rfc3339(),
        "text": text,
        "colony": None::<Value>,
        "pr_url": None::<Value>,
        "provider": None::<Value>,
    });
    announce_host(app, client, candidate, text, payload, settings, reasons).await;
}

/// Announces one out-of-quota card (issue #767) down the same channels and ledger as a provider
/// event. The fact key names the provider and its reset, so a card that flaps closed and open again
/// inside the dedup window for the same reset is one line, not two.
async fn announce_quota(
    app: &App,
    client: Option<&reqwest::Client>,
    card: &QuotaCard,
    settings: &NotifySettings,
    reasons: &mut Reasons,
) {
    let candidate = ledger::Candidate {
        kind: ledger::Kind::Notify,
        topic: format!("quota:{}", card.provider),
        class: crate::push_prefs::QUOTA.to_string(),
        fact: Some(format!("quota:{}:{}", card.provider, card.reset_at.as_deref().unwrap_or("-"))),
        colony: None,
        priority: false,
    };
    let text = quota_text(&card.name, card.reset_at.as_deref(), card.colonies.len());
    let payload = quota_payload(card, Utc::now());
    let route = PushRoute::Quota {
        provider: &card.provider,
        colonies: &card.colonies,
    };
    announce_routed(app, client, candidate, text, payload, settings, reasons, route).await;
}

/// A host-level line from another module (issue #972: the merge-train loop's CI-unavailable edges),
/// down the same channels and ledger as a provider event: `event` names it in the webhook payload
/// and `topic` keys the ledger's cooldown. Nothing goes out while the notify module is off. The
/// line names a repository and GitHub's own reason, never repository content.
pub(crate) async fn announce_line(app: &App, event: &str, topic: String, line: &str) {
    let modules = app.modules.read().await.clone();
    if !modules.notify.as_ref().is_some_and(|c| c.enabled) {
        return;
    }
    let settings = effective_notify(&modules, &OrgSettings::default());
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("colonizer/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok();
    let candidate = ledger::Candidate {
        kind: ledger::Kind::Notify,
        topic,
        class: event.to_string(),
        fact: None,
        colony: None,
        priority: false,
    };
    let text = truncate(line, MAX_TEXT);
    let payload = json!({
        "event": event,
        "at": Utc::now().to_rfc3339(),
        "text": text,
        "colony": None::<Value>,
        "pr_url": None::<Value>,
        "provider": None::<Value>,
    });
    announce_host(
        app,
        client.as_ref(),
        candidate,
        text,
        payload,
        &settings,
        &mut Reasons::default(),
    )
    .await;
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
    deliver_routed(app, client, text, payload, settings, reasons, PushRoute::Event(session)).await
}

/// Who the push channel speaks to for one announcement.
#[derive(Clone, Copy)]
enum PushRoute<'a> {
    /// The ordinary event: about one colony, or about none.
    Event(Option<&'a Session>),
    /// An out-of-quota card (issue #767): one push per provider, reaching a device when any of the
    /// card's colonies is in its scope.
    Quota { provider: &'a str, colonies: &'a [Session] },
}

/// [`deliver`], with the push route explicit.
async fn deliver_routed(
    app: &App,
    client: Option<&reqwest::Client>,
    text: &str,
    payload: &Value,
    settings: &NotifySettings,
    reasons: &mut Reasons,
    route: PushRoute<'_>,
) -> bool {
    let session = match route {
        PushRoute::Event(session) => session,
        PushRoute::Quota { .. } => None,
    };
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
    // Push carries the same event name and the same one line the other channels do, and each
    // device's own preferences decide whether it wants this event, repo and hour.
    let pushed = match route {
        PushRoute::Event(session) => {
            push::deliver(app, client, payload["event"].as_str().unwrap_or("notify"), text, session).await
        }
        PushRoute::Quota { provider, colonies } => push::deliver_quota(app, client, provider, text, colonies).await,
    };
    if pushed {
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

/// This module's background work, started once by `server::start_tasks` when the mothership serves.
pub(crate) fn start_tasks(app: &crate::Shared) {
    tokio::spawn(run(app.clone()));
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/notify/secret", routing::get(secret_status).put(put_secret))
}

#[cfg(test)]
mod tests;
