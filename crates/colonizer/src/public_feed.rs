//! The public activity feed (issue #895): a sanitized, read-only view of what this mothership's
//! colonies are doing, for a site that is not the cockpit.
//!
//! The whole point is what the feed *cannot* say. [`FeedEvent`] is a closed allowlist of ten
//! fields, and a test asserts the serialized key set is exactly those ten, so adding a field to
//! it later breaks the build rather than quietly widening what an external site can read. The
//! activity log's own `title`, `target` and `detail` — a colony's task, a failure's reason, a
//! question's text — are never copied: they are where a prompt, tool output or a quote of an issue
//! body would arrive, redacted or not. There is no title of any kind in the feed, including one
//! called `issue_title`; [`FeedEvent`] says why, and the reasoning survives renaming it. The only
//! free text left is `pr_url`, which is validated as an http(s) URL rather than trusted, and an
//! event is only published at all when its repository is on the operator's own allowlist.
//!
//! Two gates stand in front of it. `[public_feed] enabled` decides whether the routes exist (off,
//! they are a 404 — a disabled feature should not be discoverable). The repository allowlist then
//! decides what is published: an event whose repository is not listed is dropped, not blanked,
//! because a line that says "something happened, somewhere" is still a leak of this host's
//! activity, and because there is no useful anonymous version of it. `chat.*`, `remote.*`,
//! `redteam.*`, `secret.*`, `settings.*` and any line with no colony are dropped for the same
//! reason: they are about the installation rather than about a colony.
//!
//! Authentication is the feed's own ([`keys`]), never the install's API token: the whole point is
//! that a third-party site holds a read-only key for the feed and nothing else. The handler owns
//! that check, which is why the two routes are an open door in `host_guard` — the guard
//! authenticates every other request, and these two authenticate themselves. Because the key's
//! address allowlist is the second control, both routes refuse a request that arrived through the
//! remote tunnel: that request carries no peer address, so the allowlist would be silently
//! skipped rather than enforced.

pub mod keys;

use crate::{App, Shared, client_error, config::FileConfig};
use axum::{
    Json,
    extract::{ConnectInfo, Extension, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashMap, convert::Infallible, net::SocketAddr, path::Path, time::Duration};
use tokio::sync::mpsc;

/// How many events `GET /api/public/feed` answers with, and the ceiling on `history_limit`: the
/// snapshot is what a page draws from, and a site that wants more asks the stream instead.
pub(crate) const DEFAULT_HISTORY_LIMIT: usize = 200;
/// The most events one snapshot answers, whatever `history_limit` says. A public endpoint does not
/// take an unbounded read from one request.
const MAX_HISTORY_LIMIT: usize = 1000;
/// How often the stream re-reads the activity log. The same 1 s cadence `stream.rs` diffs
/// sessions on: a colony that finishes should be visible on the site about as fast as it is in
/// the cockpit.
const POLL: Duration = Duration::from_secs(1);
/// At most one `tick` per colony per this many seconds. A tick is a heartbeat, not news; a busy
/// colony writes a line every few seconds and a site animating it needs no more than one a minute.
const TICK_WINDOW_SECS: i64 = 60;
/// The longest `pr_url` published. Every real pull-request URL is far shorter; the ceiling keeps a
/// malformed one from turning a feed event into a megabyte of SSE frame.
const MAX_PR_URL: usize = 2048;
/// Bytes of colony id a colony's pseudonym keeps before the hex truncation (6 bytes = 12 hex
/// characters), which is the width `observability::hashing` uses for the same job.
const PSEUDONYM_BYTES: usize = 6;

// ---------------------------------------------------------------------------
// The event: a closed allowlist, asserted by a test.
// ---------------------------------------------------------------------------

/// What a colony is doing, in the vocabulary a site animates. Closed: a new kind is a deliberate
/// addition here, not a string read off the activity log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedKind {
    /// The colony launched.
    Started,
    /// Still working. Emitted at most once a minute per colony.
    Tick,
    /// Waiting on a person to answer a question.
    Asking,
    /// A pull request is open.
    PrOpened,
    /// The pull request merged.
    Merged,
    /// The colony failed.
    Failed,
    /// The colony stopped.
    Stopped,
}

impl FeedKind {
    /// The wire spelling, so the docs and the tests name the same words the JSON carries.
    pub fn as_str(self) -> &'static str {
        match self {
            FeedKind::Started => "started",
            FeedKind::Tick => "tick",
            FeedKind::Asking => "asking",
            FeedKind::PrOpened => "pr_opened",
            FeedKind::Merged => "merged",
            FeedKind::Failed => "failed",
            FeedKind::Stopped => "stopped",
        }
    }
}

/// One published event. **This struct is the privacy boundary of issue #895.** Everything a third
/// party can read about a colony is one of these fields, so a field added here is a field the
/// internet can read — which is why `the_serialized_field_set_is_exactly_the_ten_published_fields`
/// asserts the serialized key set, and why the mapping below builds each one by name.
///
/// What is deliberately absent, and never to be added from the activity log's `title`, `target`
/// or `detail`: the colony's task, a question's text, a failure's reason, a path, a cost.
///
/// **There is deliberately no title here, and no `issue_title` either — do not add one back.**
/// Both candidates are operator-typed free text. The activity log's `title` falls back to the
/// colony's summary, which is the task. `Session::issue_title` looks like GitHub text but is not:
/// `POST /api/sessions` copies whatever `title` the caller sent, `handoff` falls back to a chat
/// transcript's title, and a validation run builds `"Fix: {finding title}"`. Nothing on the record
/// says which of those produced the string, so the feed would publish an operator's task under a
/// reassuring field name. `redact` strips secrets, not tasks. The issue *number* stays, because a
/// number on an allowlisted repository is a link a site can already build; the words do not.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FeedEvent {
    /// `"{host}:{seq}"`, stable across restarts, and what `Last-Event-ID` resumes from.
    pub id: String,
    pub kind: FeedKind,
    /// The colony, as a pseudonym — never its id, so two published colonies cannot be joined to a
    /// colony record by anyone holding one.
    pub colony: String,
    /// Present because the repository is on the allowlist; an event from anywhere else is dropped,
    /// not published with the repository removed.
    pub repo: Option<String>,
    /// The issue number, not its title: a number links to public GitHub, the title behind it may
    /// be anything an operator typed.
    pub issue: Option<u64>,
    /// The pull request link, validated as an http(s) URL before it is published, and clipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// The colony's status at the moment the line was written, as the log names it. A word, never
    /// the error behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub ts: DateTime<Utc>,
    /// This install's stable host id (`runtime::host_id`), so a site aggregating several
    /// motherships can tell them apart without learning anything else about the machine.
    pub host: String,
    /// A coarse activity bucket a site may scale an animation by. Reserved and unset: nothing in
    /// the activity log yields an honest tokens-per-minute figure, and a number this public that
    /// nobody can defend is worse than no number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity: Option<u8>,
}

/// The colony's stable pseudonym: the first 6 bytes of the SHA-256 of its id, hex.
///
/// A colony id is already 8 random hex characters (`util::short_id`), so this is a pseudonym
/// rather than a security boundary: it stops a casual reader joining a feed event back to a
/// colony record, and nothing more. It is deliberately *not* the keyed HMAC of
/// `observability::hashing` — that lives in the separate `colonizer-observability` add-on binary,
/// which `colonizer-harness` does not depend on, and reaching it across the process boundary for
/// this would be more machinery than the field is worth.
fn pseudonym(colony: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, colony.as_bytes());
    crate::util::hex(&digest.as_ref()[..PSEUDONYM_BYTES])
}

// ---------------------------------------------------------------------------
// Config: `[public_feed]` in colonizer.toml, off by default.
// ---------------------------------------------------------------------------

/// The `[public_feed]` table of `colonizer.toml`. Absent, or `enabled = false`, means both routes
/// are a 404. Read where it is used (`FileConfig::load`), so switching the feed on or editing the
/// allowlist needs no restart.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FeedConfig {
    pub enabled: bool,
    /// `owner/name` repositories this install publishes. An empty list publishes nothing at all:
    /// every event is dropped, so a feed switched on without an allowlist is a working endpoint
    /// with an empty body rather than a public one.
    pub repos: Vec<String>,
    /// Host ids this install publishes under, for a site aggregating a fleet. Empty means this
    /// host's own id, which is what a single-install deployment wants.
    pub hosts: Vec<String>,
    /// How many events `GET /api/public/feed` answers with.
    pub history_limit: Option<usize>,
}

impl FeedConfig {
    /// How many events a snapshot holds, bounded so one request cannot ask for an unbounded read.
    fn history_limit(&self) -> usize {
        self.history_limit
            .unwrap_or(DEFAULT_HISTORY_LIMIT)
            .clamp(1, MAX_HISTORY_LIMIT)
    }

    /// Whether this host publishes: an empty `hosts` list means this install alone, a named one
    /// has to include this host's id.
    fn publishes_host(&self, host: &str) -> bool {
        self.hosts.is_empty() || self.hosts.iter().any(|h| h == host)
    }

    /// Whether `repo` is on the allowlist, compared case-insensitively the way the rest of the API
    /// compares a repository name.
    fn publishes_repo(&self, repo: &str) -> bool {
        self.repos.iter().any(|allowed| allowed.eq_ignore_ascii_case(repo))
    }
}

/// The feed's settings for this request, or the 404 that says the feed is not on. `None` is the
/// off case in every direction: `enabled = false`, a `hosts` list that excludes this host, or a
/// `colonizer.toml` that will not parse (which reads as the defaults, i.e. off).
fn settings(app: &App) -> Option<FeedConfig> {
    let cfg = FileConfig::load(&app.cfg.config_dir).public_feed;
    let host = crate::runtime::host_id(app);
    (cfg.enabled && cfg.publishes_host(&host)).then_some(cfg)
}

// ---------------------------------------------------------------------------
// The mapping: an activity line, or nothing at all.
// ---------------------------------------------------------------------------

/// The feed kind an activity kind is, or `None` for a line the feed never publishes: `chat.*` is
/// a conversation, `remote.*` and `redteam.*` are about the installation rather than a colony,
/// and `secret.*` and `settings.*` are the operator's own settings. Dropping them here as well as
/// at the projector is belt and braces — the rules below already refuse anything with no colony
/// or no repository — but a line about the install has no business in a feed of colonies.
fn kind_of(kind: &str) -> Option<FeedKind> {
    const INSTALL_LEVEL: [&str; 5] = ["chat.", "remote.", "redteam.", "secret.", "settings."];
    if INSTALL_LEVEL.iter().any(|prefix| kind.starts_with(prefix)) {
        return None;
    }
    Some(match kind {
        "colony.launch" => FeedKind::Started,
        "outcome.question" => FeedKind::Asking,
        "outcome.pr_opened" => FeedKind::PrOpened,
        "outcome.merged" => FeedKind::Merged,
        "outcome.failed" => FeedKind::Failed,
        "outcome.stopped" => FeedKind::Stopped,
        _ => FeedKind::Tick,
    })
}

/// The colony status a feed kind implies, so a site can place an entity without re-deriving the
/// vocabulary. A word, never anything from `detail`.
fn status_of(kind: FeedKind) -> Option<&'static str> {
    Some(match kind {
        FeedKind::Started | FeedKind::Tick => "running",
        FeedKind::Asking => "waiting_for_answer",
        FeedKind::PrOpened => "pr_opened",
        FeedKind::Merged => "merged",
        FeedKind::Failed => "failed",
        FeedKind::Stopped => "stopped",
    })
}

/// The pull request URL an activity line names, if it is one a site can safely link to.
///
/// Every other free-text field went through the redactor; this one was copied straight, and a
/// `pr_url` on an entry can be anything a module wrote into the log. It is validated rather than
/// trusted: an http(s) URL with a host, no whitespace or control characters (a newline would
/// split one SSE frame in two), and clipped. Anything else is published as `None` — a bad link is
/// not worth a field that a browser might resolve as `javascript:` or a data: blob.
fn pr_url_of(entry: &crate::activity::Entry) -> Option<String> {
    let raw = entry.pr_url.as_deref()?;
    let url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(crate::util::truncate(raw, MAX_PR_URL))
}

/// One activity line as a feed event, or `None` for a line the feed does not publish: no colony, a
/// repository that is not on the allowlist, or a kind that is about the installation. Dropped, not
/// blanked — see the module docs.
fn project(entry: &crate::activity::Entry, cfg: &FeedConfig, host: &str) -> Option<FeedEvent> {
    let colony = entry.colony.as_deref().filter(|c| !c.is_empty())?;
    // The repository gate comes first: an event for a repository that is not opted in is not
    // published at all, so its issue number and pull-request link never reach a projector either.
    let repo = entry.repo.as_deref().filter(|r| !r.is_empty())?;
    if !cfg.publishes_repo(repo) {
        return None;
    }
    let kind = kind_of(&entry.kind)?;
    // A line that will not parse as a time is not worth publishing with a made-up one: the site
    // would draw a colony at the wrong moment.
    let ts = DateTime::parse_from_rfc3339(&entry.ts).map(|t| t.with_timezone(&Utc)).ok()?;
    Some(FeedEvent {
        id: format!("{host}:{}", entry.seq),
        kind,
        colony: pseudonym(colony),
        repo: Some(repo.to_string()),
        issue: entry.issue,
        pr_url: pr_url_of(entry),
        status: status_of(kind).map(str::to_string),
        ts,
        host: host.to_string(),
        intensity: None,
    })
}

/// The events for a window of the activity log, newest last, with ticks thinned.
///
/// `last_tick` is the per-colony "when did this colony last publish a tick", carried in from the
/// caller rather than built here. A snapshot starts empty and gets exactly the previous per-read
/// behaviour (the whole log in one call is the window). The stream passes the same map to every
/// poll: a fresh map per poll would hold one tick per colony *per window*, and since each poll
/// reads only the newly-arrived lines the 60 s check could never fire — a busy colony would
/// publish a tick a second. The map is per-connection, in memory only; a reconnect thins from
/// whatever it replays, which is the same thing the snapshot does.
fn events_from(
    entries: &[crate::activity::Entry],
    cfg: &FeedConfig,
    host: &str,
    last_tick: &mut HashMap<String, DateTime<Utc>>,
) -> Vec<FeedEvent> {
    let mut out = Vec::new();
    for entry in entries {
        let Some(event) = project(entry, cfg, host) else {
            continue;
        };
        if event.kind == FeedKind::Tick {
            let colony = entry.colony.clone().unwrap_or_default();
            match last_tick.get(&colony) {
                // Entries are oldest first, so the newest tick of a colony in the window is the
                // one worth keeping: drop the earlier ones.
                Some(when) if event.ts - *when < chrono::Duration::seconds(TICK_WINDOW_SECS) => continue,
                _ => {}
            }
            last_tick.insert(colony, event.ts);
        }
        out.push(event);
    }
    out
}

/// The activity log read as the feed wants it: oldest first, straight from the one reader
/// `GET /api/activity` uses, so a line the cockpit can show is a line the feed can see and the two
/// never disagree about what exists.
fn read_log(data_dir: &Path) -> (Vec<crate::activity::Entry>, usize) {
    let (mut entries, skipped) = crate::activity::read_all(data_dir);
    entries.sort_by_key(|e| e.seq);
    entries.dedup_by_key(|e| e.seq);
    (entries, skipped)
}

// ---------------------------------------------------------------------------
// The routes.
// ---------------------------------------------------------------------------

/// `GET /api/public/feed`: the last N events and the colonies currently in flight.
pub async fn snapshot(
    State(app): State<Shared>,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    tunnelled: Option<Extension<crate::remote::Tunnelled>>,
    headers: HeaderMap,
) -> Result<Response, crate::AppError> {
    let cfg = settings(&app).ok_or(feed_off())?;
    refuse_tunnelled(tunnelled)?;
    authenticate(&app, connect, &headers).await?;
    let host = host_id(&app);
    let data_dir = app.cfg.data_dir.clone();
    let (entries, skipped) = tokio::task::spawn_blocking(move || read_log(&data_dir))
        .await
        .unwrap_or_default();
    if skipped > 0 {
        eprintln!("public feed: skipped {skipped} unreadable activity lines");
    }
    let limit = cfg.history_limit();
    // A snapshot reads the whole log in one call, so a fresh tick map thins exactly as it did:
    // one tick per colony per minute across the window it is answering.
    let all = events_from(&entries, &cfg, &host, &mut HashMap::new());
    let events = all[all.len().saturating_sub(limit)..].to_vec();
    Ok(Json(json!({
        "events": events,
        "active": active_colonies(&events),
    }))
    .into_response())
}

/// The colonies a site should be animating: those whose newest event in the window says they are
/// still going. A colony that merged, failed or stopped drops out, which is what an external view
/// wants and what the cockpit's own list already draws.
fn active_colonies(events: &[FeedEvent]) -> Vec<serde_json::Value> {
    // Oldest first, so the last event seen for a colony is its newest.
    let mut newest: HashMap<&str, &FeedEvent> = HashMap::new();
    for event in events {
        newest.insert(event.colony.as_str(), event);
    }
    newest
        .into_values()
        .filter(|event| !matches!(event.kind, FeedKind::Merged | FeedKind::Failed | FeedKind::Stopped))
        .map(|event| {
            json!({
                "colony": event.colony,
                "repo": event.repo,
                "issue": event.issue,
                "kind": event.kind.as_str(),
                "status": event.status,
                "since": event.ts,
            })
        })
        .collect()
}

/// `GET /api/public/feed/stream`: the same events as Server-Sent Events, with `Last-Event-ID`
/// resume.
pub async fn stream(
    State(app): State<Shared>,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    tunnelled: Option<Extension<crate::remote::Tunnelled>>,
    headers: HeaderMap,
) -> Result<Response, crate::AppError> {
    let cfg = settings(&app).ok_or(feed_off())?;
    refuse_tunnelled(tunnelled)?;
    authenticate(&app, connect, &headers).await?;
    let host = host_id(&app);
    let after = last_event_id(&headers);
    let (tx, rx) = mpsc::channel::<Frame>(256);
    tokio::spawn(pump(app, cfg, host, after, tx));
    let frames = futures_util::stream::unfold(rx, |mut rx| async move {
        let frame = rx.recv().await?;
        Some((Ok::<_, Infallible>(frame.0), rx))
    });
    Ok(Sse::new(frames).keep_alive(KeepAlive::default()).into_response())
}

/// What the pump sends: either an event or a note about the resume, wrapped so the stream itself
/// stays one type.
struct Frame(Event);

/// One event as an SSE frame. The `id` is the event's own id, so a browser's `EventSource`
/// reconnects with `Last-Event-ID` and resumes from exactly here.
fn to_sse(event: &FeedEvent) -> Event {
    Event::default()
        .id(event.id.clone())
        .event(event.kind.as_str())
        .data(serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string()))
}

/// The `seq` a reconnecting client last saw, from the `Last-Event-ID` header. Anything that does
/// not parse is `None`, which starts at the oldest retained line — the same place a client with no
/// header starts, so a malformed resume degrades to a full snapshot rather than to nothing.
fn last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit_once(':'))
        .and_then(|(_, seq)| seq.trim().parse().ok())
}

/// Feeds the stream: the retained lines after the resume point, then a poll a second for new ones.
///
/// Polling the log rather than a broadcast channel is the point: the log is already the durable,
/// already-redacted record, and a client that reconnects after an hour gets the same answer a
/// client that never disconnected would.
async fn pump(app: Shared, cfg: FeedConfig, host: String, after: Option<u64>, tx: mpsc::Sender<Frame>) {
    let mut cursor = after;
    let mut announced_gap = false;
    // Carried across polls, or the tick limit would restart at every read and never fire.
    let mut last_tick: HashMap<String, DateTime<Utc>> = HashMap::new();
    loop {
        let data_dir = app.cfg.data_dir.clone();
        let (entries, _) = tokio::task::spawn_blocking(move || read_log(&data_dir))
            .await
            .unwrap_or_default();
        // An empty read is a rotation in progress or a transient read error, not an empty log:
        // taking its bounds as 0 would rewind the cursor and replay the whole retained history at
        // a client that already has it. Leave the cursor alone and try again next second.
        let Some(oldest) = entries.first().map(|e| e.seq) else {
            tokio::time::sleep(POLL).await;
            continue;
        };
        let newest = entries.last().map(|e| e.seq).unwrap_or(oldest);
        // A resume point the log has already rotated past cannot be honoured: those lines are gone.
        // Say so rather than starting at the oldest retained line as though nothing were missing —
        // a site drawing a colony trail would silently draw a broken one.
        if !announced_gap
            && let Some(from) = cursor
            && from + 1 < oldest
        {
            announced_gap = true;
            if tx
                .send(Frame(
                    Event::default()
                        .event("gap")
                        .data(format!("{{\"requested_after\":{from},\"oldest_retained\":{oldest}}}")),
                ))
                .await
                .is_err()
            {
                return;
            }
        }
        // With no resume point a stream starts at the oldest retained line: a client that connects
        // gets the recent past and then the live tail, rather than an empty stream until the next
        // colony happens to move.
        let from = cursor.map(|c| c + 1).unwrap_or(oldest);
        let window: Vec<crate::activity::Entry> = entries.iter().filter(|e| e.seq >= from).cloned().collect();
        // Advance past every line read, published or not: a dropped line (a repository off the
        // allowlist) must not be re-examined on the next tick, nor hold the cursor back.
        cursor = Some(newest);
        for event in events_from(&window, &cfg, &host, &mut last_tick) {
            if tx.send(Frame(to_sse(&event))).await.is_err() {
                return; // the client went away
            }
        }
        tokio::time::sleep(POLL).await;
    }
}

// ---------------------------------------------------------------------------
// Authentication, in the handler: 401, then 403, then 429.
// ---------------------------------------------------------------------------

/// Whether a path is one of the feed's two routes. `host_guard` (server.rs) asks before routing,
/// so the two are an open door there and authenticate in [`authenticate`]; it cannot read the
/// router's table, which is why the paths are named here rather than in the guard.
pub(crate) fn is_feed_path(path: &str) -> bool {
    matches!(path, "/api/public/feed" | "/api/public/feed/stream")
}

/// The 404 a switched-off feed answers, so an off feed and a route that does not exist look the
/// same to a caller.
fn feed_off() -> crate::AppError {
    client_error(StatusCode::NOT_FOUND, "the public feed is not enabled on this install")
}

/// The 401 an unknown, revoked or malformed key gets — one answer for all three.
fn unauthorized() -> crate::AppError {
    client_error(
        StatusCode::UNAUTHORIZED,
        "a feed key is required: send it as `Authorization: Bearer <key>`",
    )
}

/// The 403 a key presented from outside its address allowlist gets.
fn forbidden() -> crate::AppError {
    client_error(StatusCode::FORBIDDEN, "this address is not on the feed key's ip_allowlist")
}

/// The 403 a request that arrived through the remote tunnel gets.
///
/// The address allowlist is the second control a feed key carries, and it needs a peer address to
/// mean anything. `remote::serve_connection` builds the tunneled request by hand and puts only the
/// `Tunnelled` marker on it, so [`authenticate`] would see no address, skip the allowlist entirely
/// and answer 200 — the feed reachable from anywhere the link is. A tunnel is not a fixed source
/// address, so there is nothing to allowlist against: the feed refuses it outright.
fn refuse_tunnelled(tunnelled: Option<Extension<crate::remote::Tunnelled>>) -> Result<(), crate::AppError> {
    if tunnelled.is_some() {
        return Err(client_error(
            StatusCode::FORBIDDEN,
            "the public feed is not available over the remote tunnel",
        ));
    }
    Ok(())
}

/// The 429 a key over its per-minute rate gets.
fn limited() -> crate::AppError {
    client_error(StatusCode::TOO_MANY_REQUESTS, "too many feed requests; slow down")
}

/// Checks one feed request, in the order a caller can act on: who you are, from where, how often.
///
/// `X-Forwarded-For` is deliberately not consulted. A forwarded header is a claim the client
/// makes about itself, and honouring it would let any caller name an address inside any allowlist
/// by adding a header. A feed behind a reverse proxy must therefore be reached on an address its
/// key's allowlist names, which is a deployment decision the operator makes knowingly.
async fn authenticate(
    app: &App,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: &HeaderMap,
) -> Result<keys::FeedKey, crate::AppError> {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let key = keys::authenticate(&app.cfg.config_dir, presented).ok_or_else(unauthorized)?;
    // No `ConnectInfo` means the server was not given the peer's address, so there is nothing to
    // judge the allowlist against: the key is then the whole control. Documented, not silent.
    if let Some(ip) = keys::peer_ip(connect)
        && !keys::ip_allowed(&key.ip_allowlist, ip)
    {
        return Err(forbidden());
    }
    if !keys::take_slot(&key, Utc::now().timestamp()) {
        return Err(limited());
    }
    Ok(key)
}

/// This install's stable host id: the same value the fleet and the runtime host panel use.
fn host_id(app: &App) -> String {
    crate::runtime::host_id(app)
}

/// The API routes this module serves. They are an open door in `host_guard` (server.rs) and
/// authenticate themselves, in [`authenticate`].
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new()
        .route("/api/public/feed", routing::get(snapshot))
        .route("/api/public/feed/stream", routing::get(stream))
}

/// The public feed as a migrated feature (`features.rs`). Owner-only to scoped API tokens
/// (`token_scope: None`): a scoped token is a cockpit credential and has no business on a route
/// an external site reads, so the feed is reached with a feed key and nothing else.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "public_feed",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

#[cfg(test)]
mod tests;
