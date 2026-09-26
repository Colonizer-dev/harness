//! The activity log: what happened on this mothership, one JSON line per event, behind
//! `GET /api/activity` and the cockpit's History page.
//!
//! Two kinds of line land here. **Outcomes** are a colony crossing into a state a person cares
//! about — a pull request opened, merged or closed, nothing to change, stopped, failed, or waiting
//! on an answer — recorded once, at the transition, by the same edge [`App::update_session`] already
//! tells the spend journal about. **Actions** are what somebody did through the API: launching,
//! stopping or deleting a colony, answering it, saving a provider, switching a workspace off. They
//! are recorded by one route layer ([`record_actions`]) keyed on the matched route, so no handler
//! carries logging code, plus the one command that arrives over a WebSocket (an answer).
//!
//! History used to be a reading of the colony list stamped with each colony's `updated_at`. That
//! field moves on every write — a reclaim sweep flipping `cleaned_up`, a restart marking a colony
//! stopped — so one housekeeping pass re-dated days-old outcomes to "just now" and drew them as a
//! burst of fresh, identical-looking events. A line here is written at the edge and never again, so
//! its time is when the thing happened.
//!
//! What is never written: request bodies and secret values. An action names *which* thing changed
//! (a provider id, a secret's id, a module kind) and never what it was set to; the route layer does
//! not read request bodies at all, and reads a response body only for the handful of routes whose
//! answer names what they just made (a colony, loop or run) or the pull request they opened, taking
//! a fixed few fields from it.
//!
//! The file is `activity.jsonl` in the data dir. It is bounded: past [`ROTATE_BYTES`] the current
//! file becomes `activity.jsonl.1` (replacing the previous one) and a new file starts, so the log
//! never holds more than about twice that. A lost line is a lost record, not a reason to fail the
//! action it describes, so a failed append raises the storage alert and the caller carries on — the
//! spend journal's deal.

use crate::{App, AppError, Shared, auth, client_error, sessions::Session, sessions::SessionStatus, util::append_line};
use axum::{
    Json,
    body::Body,
    extract::{FromRequestParts, MatchedPath, Query, RawPathParams, Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::Response,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    cell::RefCell,
    io::BufRead,
    path::{Path, PathBuf},
};

/// The live file, in the data dir.
pub(crate) const FILE: &str = "activity.jsonl";
/// The previous generation, kept whole until the live file fills again.
pub(crate) const ROLLED: &str = "activity.jsonl.1";
/// When the live file rolls over. About 2 MB is some ten thousand lines — months of a busy
/// install — and the pair on disk stays under 4 MB whatever happens.
pub(crate) const ROTATE_BYTES: u64 = 2 * 1024 * 1024;
/// The most entries one page answers, and the page when `limit` is not given.
pub(crate) const MAX_LIMIT: usize = 500;
const DEFAULT_LIMIT: usize = 100;
/// Free text on a line is clipped to this many characters: a target is a name, a detail one line.
const MAX_TEXT: usize = 240;

/// Every kind a line can carry, grouped by the part before the dot. Closed: the reader refuses a
/// filter naming anything else, listing these.
pub(crate) const KINDS: &[&str] = &[
    "outcome.pr_opened",
    "outcome.merged",
    "outcome.closed",
    "outcome.no_changes",
    "outcome.stopped",
    "outcome.failed",
    "outcome.question",
    "outcome.suspended",
    "outcome.restored",
    "colony.launch",
    "colony.stop",
    "colony.resume",
    "colony.delete",
    "colony.publish",
    "colony.catch_up",
    "colony.cleanup",
    "colony.retain",
    "colony.answer",
    "chat.colony",
    "chat.issue",
    "colonize.issue",
    "colonize.colony",
    "loop.create",
    "loop.update",
    "loop.pause",
    "loop.resume",
    "loop.delete",
    "loop.run_now",
    "redteam.start",
    "redteam.stop",
    "redteam.schedule",
    "redteam.unschedule",
    "remote.enable",
    "remote.disable",
    "remote.reset",
    "workspace.enable",
    "workspace.disable",
    "workspace.settings",
    "settings.save",
    "settings.remove",
    "memory.review",
    "memory.note",
    "burn_down.stop",
    "app.update",
    "map.create",
    "map.refresh",
];

/// The actors a line names: a person through the API (`you`), or the colony itself.
pub(crate) const ACTORS: &[&str] = &["you", "colony"];

/// One line of the log. Everything but `seq`, `ts`, `kind` and `actor` is optional and omitted
/// while unknown, and the reader takes `#[serde(default)]`, so a line from a newer build with a
/// field this one does not know still reads.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Entry {
    /// Increases by one per line across both files and restarts; the paging cursor.
    pub seq: u64,
    pub ts: String,
    pub kind: String,
    /// `you` (someone holding the API token) or `colony`.
    pub actor: String,
    /// How `you` reached the mothership: `cockpit` (the browser's cookie) or `api` (a bearer token,
    /// so the CLI or a script).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<u64>,
    /// The colony the line is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colony: Option<String>,
    /// What the line is about when it is not (only) a colony: `provider openrouter`, `module agent`,
    /// `secret provider-keys:openrouter`, a loop's name. A name, never a value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Where the cockpit shows the target: `providers`, `secrets`, `module:agent`, `loops`,
    /// `org:<org>`, `connections`, `updates`, `usage`, `redteam`, `memory`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// The colony's task, so a line still reads after the colony is deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// One line of context: a failure's reason, the loop a colony came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Entry {
    pub(crate) fn new(kind: &str, actor: &str) -> Self {
        Entry {
            kind: kind.to_string(),
            actor: actor.to_string(),
            ..Entry::default()
        }
    }

    /// Fills the colony fields from its record.
    pub(crate) fn colony(mut self, s: &Session) -> Self {
        self.colony = Some(s.id.clone());
        self.org = Some(s.org.clone()).filter(|o| !o.is_empty()).or_else(|| owner_of(&s.repo));
        self.repo = Some(s.repo.clone()).filter(|r| !r.is_empty());
        self.issue = s.issue;
        let title = if s.issue_title.trim().is_empty() {
            s.summary.as_deref().unwrap_or_default()
        } else {
            s.issue_title.as_str()
        };
        self.title = Some(title.trim().to_string()).filter(|t| !t.is_empty());
        self.pr_url = s.pr_url.clone();
        self
    }

    fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    fn section(mut self, section: impl Into<String>) -> Self {
        self.section = Some(section.into());
        self
    }

    /// Clips every free-text field, so no line can carry a paragraph.
    fn clipped(mut self) -> Self {
        for field in [&mut self.target, &mut self.title, &mut self.detail] {
            if let Some(text) = field.as_mut() {
                let one_line = text.replace(['\n', '\r'], " ");
                *text = crate::util::truncate(one_line.trim(), MAX_TEXT);
            }
        }
        self
    }
}

fn owner_of(repo: &str) -> Option<String> {
    repo.split_once('/')
        .map(|(owner, _)| owner.to_string())
        .filter(|o| !o.is_empty())
}

/// The log's in-memory half: the next `seq`, found by scanning the files on first use.
#[derive(Default)]
pub struct ActivityLog {
    next_seq: tokio::sync::Mutex<Option<u64>>,
}

impl ActivityLog {
    pub fn new() -> Self {
        Self::default()
    }
}

fn live_file(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

fn rolled_file(data_dir: &Path) -> PathBuf {
    data_dir.join(ROLLED)
}

/// Every readable line of one file, oldest first, and how many lines would not read. A file that is
/// not there has no lines.
fn read_file(path: &Path) -> (Vec<Entry>, usize) {
    let Ok(file) = std::fs::File::open(path) else {
        return (Vec::new(), 0);
    };
    let mut skipped = 0;
    let mut out = Vec::new();
    for line in std::io::BufReader::new(file).split(b'\n').map_while(Result::ok) {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match std::str::from_utf8(&line)
            .ok()
            .and_then(|l| serde_json::from_str::<Entry>(l).ok())
        {
            Some(entry) if !entry.kind.is_empty() && entry.seq > 0 => out.push(entry),
            _ => skipped += 1,
        }
    }
    (out, skipped)
}

/// Both files, oldest first, and the count of lines that would not read.
fn read_all(data_dir: &Path) -> (Vec<Entry>, usize) {
    let (mut entries, skipped_old) = read_file(&rolled_file(data_dir));
    let (live, skipped_live) = read_file(&live_file(data_dir));
    entries.extend(live);
    (entries, skipped_old + skipped_live)
}

/// Appends one line, stamping its `seq` and (when unset) its time. Rolls the live file first when
/// it has reached [`ROTATE_BYTES`].
pub(crate) async fn record(app: &App, entry: Entry) {
    record_with_limit(app, entry, ROTATE_BYTES).await;
}

async fn record_with_limit(app: &App, entry: Entry, rotate_bytes: u64) {
    let data_dir = app.cfg.data_dir.clone();
    let mut next = app.activity.next_seq.lock().await;
    let seq = match *next {
        Some(seq) => seq,
        None => {
            let dir = data_dir.clone();
            let last = tokio::task::spawn_blocking(move || read_all(&dir).0.iter().map(|e| e.seq).max().unwrap_or(0))
                .await
                .unwrap_or(0);
            last + 1
        }
    };
    let mut entry = entry.clipped();
    entry.seq = seq;
    if entry.ts.is_empty() {
        entry.ts = Utc::now().to_rfc3339();
    }
    let Ok(line) = serde_json::to_string(&entry) else {
        return; // a fixed-shape line cannot fail to serialize
    };
    let live = live_file(&data_dir);
    if tokio::fs::metadata(&live).await.is_ok_and(|m| m.len() >= rotate_bytes)
        && let Err(e) = tokio::fs::rename(&live, rolled_file(&data_dir)).await
    {
        // Appending on past the cap is the lesser harm: the line is kept, the file grows until
        // the next roll works, and the alert says why.
        app.storage_failed("roll the activity log over", &anyhow::Error::from(e))
            .await;
    }
    match append_line(&live, &line).await {
        Ok(()) => *next = Some(seq + 1),
        Err(e) => {
            // The seq is not consumed, and the next append retries the scan-free counter.
            *next = Some(seq);
            app.storage_failed("append to the activity log", &e).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Outcomes: the transition edge.
// ---------------------------------------------------------------------------

/// The outcome kind a status is, or `None` for the statuses in between (queued, starting, running,
/// idle, publishing), which are not things that happened *to* a person's work.
pub(crate) fn outcome_kind(status: SessionStatus) -> Option<&'static str> {
    Some(match status {
        SessionStatus::PrOpened => "outcome.pr_opened",
        SessionStatus::Merged => "outcome.merged",
        SessionStatus::Closed => "outcome.closed",
        SessionStatus::NoChanges => "outcome.no_changes",
        SessionStatus::Stopped => "outcome.stopped",
        SessionStatus::Failed => "outcome.failed",
        SessionStatus::WaitingForAnswer => "outcome.question",
        _ => return None,
    })
}

/// The line for a colony that moved from `before` to its current status, or `None` when the move
/// is not an outcome — no status change at all (a housekeeping write: `cleaned_up`, the app slot,
/// a measurement) or a change into an in-between status. Pure, so the rule is tested apart from the
/// file.
pub(crate) fn outcome_entry(before: SessionStatus, after: &Session) -> Option<Entry> {
    if before == after.status {
        return None;
    }
    let kind = outcome_kind(after.status)?;
    let mut entry = Entry::new(kind, "colony").colony(after);
    if after.status == SessionStatus::Failed || after.status == SessionStatus::Stopped {
        entry.detail = after.error.clone().filter(|e| !e.trim().is_empty());
    }
    Some(entry)
}

/// Records a colony's transition from `before`, when it is an outcome. Called by
/// [`App::update_session`] and by the queue's retire, the two places a status changes.
///
/// When the transition happens inside an API request that is itself recorded (a Stop press sets
/// `stopped` before the handler returns), the outcome line takes the person as its actor and the
/// request writes no second line: one press, one line.
pub(crate) async fn record_transition(app: &App, before: SessionStatus, after: &Session) {
    let Some(mut entry) = outcome_entry(before, after) else {
        return;
    };
    if let Some((actor, via)) = absorb(&after.id) {
        entry.actor = actor;
        entry.via = via;
    }
    record(app, entry).await;
}

// ---------------------------------------------------------------------------
// Actions: the route layer.
// ---------------------------------------------------------------------------

/// What a recorded route is about, which decides how its line is filled in.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    /// `{id}` names a colony; its record is read before the handler runs (a delete forgets it).
    Colony,
    /// The response is the colony just made.
    NewColony,
    /// `{id}` names a loop.
    Loop,
    /// The response is the loop just made.
    NewLoop,
    /// `{org}` names a workspace; its `enabled` is compared before and after.
    Workspace,
    /// The response is the red-team run just started.
    NewRun,
    /// The response is the issue just filed (`{repo, number, title}`).
    NewIssue,
    /// Something named by a label and the route's `{id}`/`{kind}` when it has one, shown in a
    /// cockpit section (`module:` is completed with the module's kind).
    Named(&'static str, &'static str),
    /// A fixed description; the route's `{id}` (a proposal or note id) says nothing to a person.
    Fixed(&'static str, &'static str),
    /// No target beyond the kind.
    None,
}

/// One recorded route: method, the route as its module's `routes()` registers it, the kind its line gets and what
/// it is about. Routes not listed here are not recorded: reads, and writes that are not a person
/// changing something (chat messages, drafts, uploads, probes).
struct Rule {
    method: &'static str,
    route: &'static str,
    kind: &'static str,
    target: Target,
}

const fn rule(method: &'static str, route: &'static str, kind: &'static str, target: Target) -> Rule {
    Rule {
        method,
        route,
        kind,
        target,
    }
}

const RULES: &[Rule] = &[
    rule("POST", "/api/sessions", "colony.launch", Target::NewColony),
    rule("POST", "/api/sessions/{id}/stop", "colony.stop", Target::Colony),
    rule("POST", "/api/sessions/{id}/resume", "colony.resume", Target::Colony),
    rule("DELETE", "/api/sessions/{id}", "colony.delete", Target::Colony),
    rule("POST", "/api/sessions/{id}/publish", "colony.publish", Target::Colony),
    rule("POST", "/api/sessions/{id}/catch-up", "colony.catch_up", Target::Colony),
    rule("POST", "/api/sessions/{id}/cleanup", "colony.cleanup", Target::Colony),
    rule("POST", "/api/sessions/{id}/retain", "colony.retain", Target::Colony),
    rule("POST", "/api/chat/{id}/issue", "chat.issue", Target::None),
    rule("POST", "/api/repos/{owner}/{name}/issues", "colonize.issue", Target::NewIssue),
    rule("POST", "/api/loops", "loop.create", Target::NewLoop),
    rule("PUT", "/api/loops/{id}", "loop.update", Target::Loop),
    rule("DELETE", "/api/loops/{id}", "loop.delete", Target::Loop),
    rule("POST", "/api/loops/{id}/run-now", "loop.run_now", Target::Loop),
    rule("POST", "/api/redteam/runs", "redteam.start", Target::NewRun),
    rule(
        "POST",
        "/api/redteam/runs/{id}/stop",
        "redteam.stop",
        Target::Named("red-team run", "redteam"),
    ),
    rule(
        "POST",
        "/api/redteam/schedules",
        "redteam.schedule",
        Target::Named("red-team schedule", "redteam"),
    ),
    rule(
        "PUT",
        "/api/redteam/schedules/{id}",
        "redteam.schedule",
        Target::Named("red-team schedule", "redteam"),
    ),
    rule(
        "DELETE",
        "/api/redteam/schedules/{id}",
        "redteam.unschedule",
        Target::Named("red-team schedule", "redteam"),
    ),
    rule("PUT", "/api/orgs/{org}", "workspace.settings", Target::Workspace),
    rule(
        "PUT",
        "/api/modules/{kind}",
        "settings.save",
        Target::Named("module", "module:"),
    ),
    rule(
        "PUT",
        "/api/providers/{id}",
        "settings.save",
        Target::Named("provider", "providers"),
    ),
    rule(
        "DELETE",
        "/api/providers/{id}",
        "settings.remove",
        Target::Named("provider", "providers"),
    ),
    rule(
        "PUT",
        "/api/secrets/{id}",
        "settings.save",
        Target::Named("secret", "secrets"),
    ),
    rule(
        "DELETE",
        "/api/secrets/{id}",
        "settings.remove",
        Target::Named("secret", "secrets"),
    ),
    rule(
        "POST",
        "/api/secrets/{id}/move",
        "settings.save",
        Target::Named("secret (moved)", "secrets"),
    ),
    rule(
        "POST",
        "/api/secrets/colony",
        "settings.save",
        Target::Named("colony secret", "secrets"),
    ),
    rule(
        "POST",
        "/api/settings/github-token",
        "settings.save",
        Target::Named("GitHub token", "connections"),
    ),
    rule(
        "DELETE",
        "/api/settings/github-token",
        "settings.remove",
        Target::Named("GitHub token", "connections"),
    ),
    rule(
        "POST",
        "/api/settings/claude-token",
        "settings.save",
        Target::Named("Claude token", "connections"),
    ),
    rule(
        "DELETE",
        "/api/settings/claude-token",
        "settings.remove",
        Target::Named("Claude token", "connections"),
    ),
    rule(
        "POST",
        "/api/claude-accounts",
        "settings.save",
        Target::Named("Claude account", "connections"),
    ),
    rule(
        "DELETE",
        "/api/claude-accounts/{id}",
        "settings.remove",
        Target::Named("Claude account", "connections"),
    ),
    rule(
        "PUT",
        "/api/telemetry",
        "settings.save",
        Target::Named("live map", "live-map"),
    ),
    rule(
        "PUT",
        "/api/telemetry/usage",
        "settings.save",
        Target::Named("usage reporting", "usage"),
    ),
    rule(
        "PUT",
        "/api/update",
        "settings.save",
        Target::Named("update checks", "updates"),
    ),
    rule(
        "POST",
        "/api/update/apply",
        "app.update",
        Target::Named("Colonizer", "updates"),
    ),
    rule(
        "PUT",
        "/api/memory/mem0",
        "settings.save",
        Target::Named("mem0 key", "memory"),
    ),
    rule(
        "PUT",
        "/api/notify/secret",
        "settings.save",
        Target::Named("notification secret", "notifications"),
    ),
    rule(
        "PUT",
        "/api/voice/key",
        "settings.save",
        Target::Named("voice key", "module:voice"),
    ),
    rule(
        "POST",
        "/api/login-item",
        "settings.save",
        Target::Named("start at login", "desktop"),
    ),
    rule(
        "POST",
        "/api/memory/proposals/{id}/approve",
        "memory.review",
        Target::Fixed("approved a memory proposal", "memory"),
    ),
    rule(
        "POST",
        "/api/memory/proposals/{id}/reject",
        "memory.review",
        Target::Fixed("rejected a memory proposal", "memory"),
    ),
    rule(
        "POST",
        "/api/memory/notes",
        "memory.note",
        Target::Fixed("added a memory note", "memory"),
    ),
    rule(
        "DELETE",
        "/api/memory/notes/{id}",
        "memory.note",
        Target::Fixed("removed a memory note", "memory"),
    ),
    rule(
        "POST",
        "/api/burn-down/stop",
        "burn_down.stop",
        Target::Fixed("burn-down", ""),
    ),
    rule("POST", "/api/maps/{owner}/{name}", "map.create", Target::None),
];

/// The activity kind a route records, if any, for the route-table snapshot (server.rs's tests).
#[cfg(test)]
pub(crate) fn recorded_kind(method: &Method, route: &str) -> Option<&'static str> {
    rule_for(method, route).map(|r| r.kind)
}

fn rule_for(method: &Method, route: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.method == method.as_str() && r.route == route)
}

/// What a request carries into the handler for the transition edge: who is acting, and the colonies
/// whose outcome line took that actor (so the request writes no second line for them).
struct Acting {
    actor: String,
    via: Option<String>,
    absorbed: Vec<String>,
}

tokio::task_local! {
    static ACTING: RefCell<Acting>;
}

/// Inside a recorded request: marks `colony` as told, and answers who is acting.
fn absorb(colony: &str) -> Option<(String, Option<String>)> {
    ACTING
        .try_with(|acting| {
            let mut acting = acting.borrow_mut();
            acting.absorbed.push(colony.to_string());
            (acting.actor.clone(), acting.via.clone())
        })
        .ok()
}

pub(crate) fn via_name(via: Option<auth::Via>) -> Option<String> {
    via.map(|v| match v {
        auth::Via::Cockpit => "cockpit".to_string(),
        auth::Via::Api => "api".to_string(),
        // A scoped API token acts under its name, never under its secret: `token:<name>` is what
        // History shows, and the name is all the log knows.
        auth::Via::Token(name) => format!("token:{name}"),
    })
}

/// The state a recorded route's line needs from before the handler ran.
#[derive(Default)]
struct Before {
    colony: Option<Session>,
    loop_: Option<crate::loops::Loop>,
    enabled: Option<bool>,
}

/// The route layer: runs the handler, and for a recorded route that succeeded, writes its line.
/// Registered with `route_layer` inside `host_guard`, so only authenticated requests reach it and
/// `MatchedPath` names the route.
pub(crate) async fn record_actions(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let Some(rule) = req
        .extensions()
        .get::<MatchedPath>()
        .and_then(|m| rule_for(req.method(), m.as_str()))
    else {
        return next.run(req).await;
    };
    let via = via_name(req.extensions().get::<auth::Via>().cloned());
    let (mut parts, body) = req.into_parts();
    let params: Vec<(String, String)> = RawPathParams::from_request_parts(&mut parts, &())
        .await
        .map(|p| p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
        .unwrap_or_default();
    let req = Request::from_parts(parts, body);
    let param = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());

    let before = match rule.target {
        Target::Colony => Before {
            colony: app.session(&param("id").unwrap_or_default()).await,
            ..Before::default()
        },
        Target::Loop => Before {
            loop_: app.loops.get(&param("id").unwrap_or_default()).await,
            ..Before::default()
        },
        Target::Workspace => Before {
            enabled: Some(crate::orgs::org_enabled(&app.org_settings(&param("org").unwrap_or_default()))),
            ..Before::default()
        },
        _ => Before::default(),
    };

    let acting = RefCell::new(Acting {
        actor: "you".into(),
        via: via.clone(),
        absorbed: Vec::new(),
    });
    let (response, absorbed) = ACTING
        .scope(acting, async move {
            let response = next.run(req).await;
            let absorbed = ACTING.with(|a| a.borrow().absorbed.clone());
            (response, absorbed)
        })
        .await;
    if !response.status().is_success() {
        return response;
    }
    let reads_body = matches!(
        rule.target,
        Target::NewColony | Target::NewLoop | Target::NewRun | Target::NewIssue
    ) || rule.kind == "loop.run_now"
        || rule.kind == "colony.publish";
    let (response, body) = if reads_body {
        buffer_json(response).await
    } else {
        (response, None)
    };

    let mut entry = Entry::new(rule.kind, "you");
    entry.via = via;
    match rule.target {
        Target::Colony => {
            let Some(colony) = before.colony.as_ref() else {
                return response;
            };
            if absorbed.contains(&colony.id) {
                // The handler's own transition already wrote this press's line (see
                // `record_transition`).
                return response;
            }
            entry = entry.colony(colony);
            if rule.kind == "colony.publish"
                && let Some(url) = body.as_ref().and_then(|b| b["pr_url"].as_str())
            {
                entry.pr_url = Some(url.to_string());
            }
        }
        Target::NewColony => {
            // The answer names the colony just made; its record is the source of truth, and the
            // answer's own fields stand in only if it is already gone.
            let Some(made) = body.as_ref().filter(|b| b["id"].is_string()) else {
                return response;
            };
            match made["origin"].as_str() {
                Some(CHAT_ORIGIN) => entry.kind = "chat.colony".into(),
                Some(COLONIZE_ORIGIN) => entry.kind = "colonize.colony".into(),
                _ => {}
            }
            match app.session(made["id"].as_str().unwrap_or_default()).await {
                Some(session) => entry = entry.colony(&session),
                None => {
                    entry.colony = made["id"].as_str().map(str::to_string);
                    entry.repo = made["repo"].as_str().map(str::to_string);
                    entry.org = entry.repo.as_deref().and_then(owner_of);
                    entry.issue = made["issue"].as_u64();
                }
            }
        }
        Target::Loop => {
            let Some(before) = before.loop_.as_ref() else {
                return response;
            };
            entry = entry.target(before.name.clone()).section("loops");
            entry.org = owner_of(&before.repo);
            entry.repo = Some(before.repo.clone());
            if rule.kind == "loop.update"
                && let Some(after) = app.loops.get(&before.id).await
            {
                entry.kind = match (before.enabled, after.enabled) {
                    (true, false) => "loop.pause".into(),
                    (false, true) => "loop.resume".into(),
                    _ => "loop.update".into(),
                };
            }
            if rule.kind == "loop.run_now"
                && let Some(id) = body.as_ref().and_then(|b| b["id"].as_str())
            {
                entry.colony = Some(id.to_string());
                entry.issue = body.as_ref().and_then(|b| b["issue"].as_u64());
            }
        }
        Target::NewLoop => {
            let Some(created) = body.as_ref() else { return response };
            entry = entry
                .target(created["name"].as_str().unwrap_or("a loop").to_string())
                .section("loops");
            if let Some(repo) = created["repo"].as_str() {
                entry.org = owner_of(repo);
                entry.repo = Some(repo.to_string());
            }
        }
        Target::Workspace => {
            let org = param("org").unwrap_or_default();
            let after = crate::orgs::org_enabled(&app.org_settings(&org));
            entry.kind = match (before.enabled, after) {
                (Some(true), false) => "workspace.disable".into(),
                (Some(false), true) => "workspace.enable".into(),
                _ => "workspace.settings".into(),
            };
            entry = entry.target(org.clone()).section(format!("org:{org}"));
            entry.org = Some(org);
        }
        Target::NewRun => {
            let Some(run) = body.as_ref() else { return response };
            if let Some(repo) = run["repo"].as_str() {
                entry.org = owner_of(repo);
                entry.repo = Some(repo.to_string());
            }
            entry = entry.target("red-team run").section("redteam");
        }
        Target::NewIssue => {
            let Some(made) = body.as_ref() else { return response };
            entry.repo = made["repo"].as_str().map(str::to_string);
            entry.org = entry.repo.as_deref().and_then(owner_of);
            entry.issue = made["number"].as_u64();
            entry.title = made["title"].as_str().map(str::to_string);
        }
        Target::Named(label, section) => {
            let name = param("id").or_else(|| param("kind"));
            entry = entry.target(match &name {
                Some(name) => format!("{label} {name}"),
                None => label.to_string(),
            });
            let section = if section == "module:" {
                format!("module:{}", name.unwrap_or_default())
            } else {
                section.to_string()
            };
            entry = entry.section(section);
        }
        Target::Fixed(label, section) => {
            entry = entry.target(label);
            if !section.is_empty() {
                entry = entry.section(section);
            }
        }
        Target::None => {
            if let (Some(owner), Some(name)) = (param("owner"), param("name")) {
                entry.repo = Some(format!("{owner}/{name}"));
                entry.org = Some(owner);
            }
        }
    }
    record(&app, entry).await;
    response
}

/// The `origin` the cockpit's chat sends when it turns a conversation into a colony.
pub(crate) const CHAT_ORIGIN: &str = "chat";
/// The `origin` the cockpit's Colonize pane sends when it hands issues off to colonies.
pub(crate) const COLONIZE_ORIGIN: &str = "colonize";

/// The response's JSON, for the few routes whose answer names what they made. The body is ours,
/// already in memory, so buffering it costs a copy; anything that is not JSON passes through as it
/// was.
async fn buffer_json(response: Response) -> (Response, Option<Value>) {
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        return (Response::from_parts(parts, Body::empty()), None);
    };
    let value = serde_json::from_slice::<Value>(&bytes).ok();
    (Response::from_parts(parts, Body::from(bytes)), value)
}

/// Records a person answering a colony's question, which arrives over the colony's WebSocket
/// rather than as a request the route layer sees.
pub(crate) async fn record_answer(app: &App, colony: &Session, via: Option<auth::Via>) {
    let mut entry = Entry::new("colony.answer", "you").colony(colony);
    entry.via = via_name(via);
    record(app, entry).await;
}

/// Records the harness bringing a suspended colony back to deliver its answer (issue #562): the
/// boot path calls it once the runner is up, which is when the answer counts as delivered.
pub(crate) async fn record_restored(app: &App, colony: &Session) {
    let entry = Entry::new("outcome.restored", "colony").colony(colony);
    record(app, entry).await;
}

// ---------------------------------------------------------------------------
// The reader: `GET /api/activity`.
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct ListQuery {
    /// Only lines older than this `seq` (the previous page's `next_before`).
    before: Option<u64>,
    limit: Option<usize>,
    /// Comma-separated kinds or kind groups (`outcome`, `colony`, `settings`, …).
    kind: Option<String>,
    actor: Option<String>,
    /// A workspace: its own lines plus the lines that belong to none (settings are install-wide).
    org: Option<String>,
    repo: Option<String>,
    /// Case-insensitive text over the repo, title, target, detail and colony id.
    q: Option<String>,
}

/// The kind groups a filter may name: the part of each kind before the dot.
fn kind_groups() -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = KINDS.iter().filter_map(|k| k.split('.').next()).collect();
    groups.dedup();
    groups
}

/// A parsed, validated filter. Every refusal names the parameter, the value and what is accepted.
#[derive(Debug, Default)]
struct Filter {
    before: Option<u64>,
    limit: usize,
    kinds: Vec<String>,
    actor: Option<String>,
    org: Option<String>,
    repo: Option<String>,
    q: Option<String>,
}

fn parse_filter(query: ListQuery) -> Result<Filter, String> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(format!("limit is {limit}; use a number from 1 to {MAX_LIMIT}"));
    }
    let groups = kind_groups();
    let mut kinds = Vec::new();
    for raw in query.kind.as_deref().unwrap_or_default().split(',') {
        let kind = raw.trim();
        if kind.is_empty() {
            continue;
        }
        if !KINDS.contains(&kind) && !groups.contains(&kind) {
            return Err(format!(
                "unknown activity kind \"{kind}\"; use a kind such as colony.launch, or one of the groups {}",
                groups.join(", ")
            ));
        }
        kinds.push(kind.to_string());
    }
    let actor = query.actor.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
    if let Some(actor) = &actor
        && !ACTORS.contains(&actor.as_str())
    {
        return Err(format!("unknown actor \"{actor}\"; use {}", ACTORS.join(" or ")));
    }
    let clean = |v: Option<String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    Ok(Filter {
        before: query.before,
        limit,
        kinds,
        actor,
        org: clean(query.org),
        repo: clean(query.repo),
        q: clean(query.q).map(|q| q.to_lowercase()),
    })
}

fn matches(entry: &Entry, filter: &Filter) -> bool {
    if filter.before.is_some_and(|before| entry.seq >= before) {
        return false;
    }
    if !filter.kinds.is_empty()
        && !filter
            .kinds
            .iter()
            .any(|k| entry.kind == *k || entry.kind.split('.').next() == Some(k.as_str()))
    {
        return false;
    }
    if filter.actor.as_ref().is_some_and(|a| entry.actor != *a) {
        return false;
    }
    if let Some(org) = &filter.org
        && entry.org.as_ref().is_some_and(|o| !o.eq_ignore_ascii_case(org))
    {
        return false;
    }
    if filter
        .repo
        .as_ref()
        .is_some_and(|repo| !entry.repo.as_ref().is_some_and(|r| r.eq_ignore_ascii_case(repo)))
    {
        return false;
    }
    if let Some(q) = &filter.q {
        let issue = entry.issue.map(|i| format!("#{i}"));
        let hay = [&entry.repo, &entry.title, &entry.target, &entry.detail, &entry.colony, &issue];
        if !hay
            .iter()
            .any(|f| f.as_ref().is_some_and(|f| f.to_lowercase().contains(q.as_str())))
        {
            return false;
        }
    }
    true
}

/// One page of the log, newest first: `entries`, the cursor for the next page (null on the last),
/// and how many unreadable lines the read skipped (reported, never silently dropped).
#[derive(Serialize, Debug)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next_before: Option<u64>,
    pub skipped: usize,
}

fn page(data_dir: &Path, filter: &Filter) -> Page {
    let (entries, skipped) = read_all(data_dir);
    let mut out = Vec::new();
    let mut more = false;
    // Files are appended in `seq` order, so newest first is the reverse; a line out of order (a
    // hand-edited file) is sorted rather than trusted.
    let mut entries = entries;
    entries.sort_by_key(|e| std::cmp::Reverse(e.seq));
    entries.dedup_by_key(|e| e.seq);
    for entry in entries {
        if !matches(&entry, filter) {
            continue;
        }
        if out.len() == filter.limit {
            more = true;
            break;
        }
        out.push(entry);
    }
    let next_before = if more { out.last().map(|e| e.seq) } else { None };
    Page {
        entries: out,
        next_before,
        skipped,
    }
}

/// `GET /api/activity`: see docs/protocol.md.
pub async fn list(State(app): State<Shared>, Query(query): Query<ListQuery>) -> Result<Json<Page>, AppError> {
    let filter = parse_filter(query).map_err(|message| client_error(StatusCode::BAD_REQUEST, &message))?;
    let data_dir = app.cfg.data_dir.clone();
    let page = tokio::task::spawn_blocking(move || page(&data_dir, &filter))
        .await
        .map_err(|e| {
            crate::AppError(
                StatusCode::INTERNAL_SERVER_ERROR,
                anyhow::anyhow!("could not read the activity log: {e}"),
            )
        })?;
    if page.skipped > 0 {
        eprintln!(
            "activity: skipped {} unreadable {} in {}",
            page.skipped,
            if page.skipped == 1 { "line" } else { "lines" },
            app.cfg.data_dir.join(FILE).display()
        );
    }
    Ok(Json(page))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/activity", routing::get(list))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_app;
    use axum::{Router, routing::post};
    use tower::ServiceExt;

    fn root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-activity-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn colony(id: &str, status: SessionStatus) -> Session {
        Session {
            id: id.into(),
            repo: "acme/web".into(),
            org: "acme".into(),
            issue: Some(12),
            issue_title: "Fix the checkout".into(),
            status,
            ..Session::default()
        }
    }

    fn all(app: &App) -> Vec<Entry> {
        let mut entries = read_all(&app.cfg.data_dir).0;
        entries.sort_by_key(|e| e.seq);
        entries
    }

    #[test]
    fn only_a_status_change_into_an_outcome_is_one() {
        let done = colony("a", SessionStatus::NoChanges);
        assert_eq!(
            outcome_entry(SessionStatus::Running, &done).unwrap().kind,
            "outcome.no_changes"
        );
        // The same status again is a housekeeping write (cleaned_up, the app slot), not an event.
        assert!(outcome_entry(SessionStatus::NoChanges, &done).is_none());
        // In-between statuses are not outcomes.
        assert!(outcome_entry(SessionStatus::Queued, &colony("a", SessionStatus::Starting)).is_none());
        let asked = outcome_entry(SessionStatus::Running, &colony("a", SessionStatus::WaitingForAnswer)).unwrap();
        assert_eq!((asked.kind.as_str(), asked.actor.as_str()), ("outcome.question", "colony"));
        assert_eq!(asked.repo.as_deref(), Some("acme/web"));
        assert_eq!(asked.title.as_deref(), Some("Fix the checkout"));
    }

    /// The History page's duplicates: a sweep that rewrites terminal colonies (a reclaim flipping
    /// `cleaned_up`, a restart re-marking a colony stopped) must not add a line, so an outcome is on
    /// the record once, at the time it happened.
    #[tokio::test]
    async fn housekeeping_writes_on_a_finished_colony_add_no_lines() {
        let root = root();
        let app = test_app(&root);
        app.sessions.write().await.push(colony("a", SessionStatus::Running));
        app.update_session("a", |s| s.status = SessionStatus::NoChanges).await;
        app.update_session("a", |s| s.cleaned_up = true).await;
        app.update_session("a", |s| s.app_slot = Some("/app-b".into())).await;
        app.update_session("a", |s| s.status = SessionStatus::NoChanges).await;
        let lines = all(&app);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].kind, "outcome.no_changes");
        assert_eq!(lines[0].colony.as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn seq_survives_a_restart_and_rotation_keeps_the_log_bounded() {
        let root = root();
        let app = test_app(&root);
        for n in 0..40 {
            record_with_limit(
                &app,
                Entry::new("settings.save", "you").target(format!("provider p{n}")),
                1_000,
            )
            .await;
        }
        let live = std::fs::metadata(root.join("data").join(FILE)).unwrap().len();
        let rolled = std::fs::metadata(root.join("data").join(ROLLED)).unwrap().len();
        assert!(live < 1_200 && rolled < 1_200, "live {live}, rolled {rolled}");
        // A fresh App (a restart) continues the numbering from what is on disk.
        let again = test_app(&root);
        record(&again, Entry::new("settings.save", "you")).await;
        let seqs: Vec<u64> = all(&again).iter().map(|e| e.seq).collect();
        assert_eq!(seqs.last(), Some(&41));
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    }

    #[tokio::test]
    async fn pages_newest_first_with_a_cursor_and_filters() {
        let root = root();
        let app = test_app(&root);
        for n in 0..5 {
            let mut entry = Entry::new(
                if n % 2 == 0 { "colony.launch" } else { "outcome.merged" },
                if n % 2 == 0 { "you" } else { "colony" },
            );
            entry.repo = Some(if n < 3 { "acme/web".into() } else { "acme/api".into() });
            entry.org = Some("acme".into());
            record(&app, entry).await;
        }
        record(&app, Entry::new("settings.save", "you").target("provider openrouter")).await;
        let dir = &app.cfg.data_dir;
        let first = page(
            dir,
            &parse_filter(ListQuery {
                limit: Some(2),
                ..Default::default()
            })
            .unwrap(),
        );
        assert_eq!(first.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![6, 5]);
        assert_eq!(first.next_before, Some(5));
        let second = page(
            dir,
            &parse_filter(ListQuery {
                limit: Some(10),
                before: first.next_before,
                ..Default::default()
            })
            .unwrap(),
        );
        assert_eq!(second.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![4, 3, 2, 1]);
        assert_eq!(second.next_before, None);

        let outcomes = page(
            dir,
            &parse_filter(ListQuery {
                kind: Some("outcome".into()),
                ..Default::default()
            })
            .unwrap(),
        );
        assert_eq!(outcomes.entries.len(), 2);
        let mine = page(
            dir,
            &parse_filter(ListQuery {
                actor: Some("you".into()),
                repo: Some("acme/web".into()),
                ..Default::default()
            })
            .unwrap(),
        );
        assert_eq!(mine.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 1]);
        // A workspace view keeps the install-wide lines (settings belong to no org).
        let other_org = page(
            dir,
            &parse_filter(ListQuery {
                org: Some("other".into()),
                ..Default::default()
            })
            .unwrap(),
        );
        assert_eq!(
            other_org.entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            vec!["settings.save"]
        );
        let searched = page(
            dir,
            &parse_filter(ListQuery {
                q: Some("OPENROUTER".into()),
                ..Default::default()
            })
            .unwrap(),
        );
        assert_eq!(searched.entries.len(), 1);
    }

    #[test]
    fn a_bad_filter_is_refused_by_name() {
        let err = parse_filter(ListQuery {
            kind: Some("colony.launch,colonie".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("\"colonie\"") && err.contains("outcome, colony"), "{err}");
        let err = parse_filter(ListQuery {
            actor: Some("bot".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("\"bot\"") && err.contains("you or colony"), "{err}");
        let err = parse_filter(ListQuery {
            limit: Some(0),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("limit is 0") && err.contains("1 to 500"), "{err}");
    }

    #[test]
    fn an_unreadable_line_is_counted_not_fatal() {
        let root = root();
        std::fs::write(
            root.join(FILE),
            "{\"seq\":1,\"ts\":\"t\",\"kind\":\"colony.launch\",\"actor\":\"you\"}\nnot json\n\u{0}\u{1}\n{\"seq\":2,\"ts\":\"t\",\"kind\":\"colony.stop\",\"actor\":\"you\",\"future\":true}\n",
        )
        .unwrap();
        let page = page(&root, &parse_filter(ListQuery::default()).unwrap());
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.skipped, 2);
    }

    #[test]
    fn every_kind_a_rule_writes_is_in_the_vocabulary() {
        for rule in RULES {
            assert!(KINDS.contains(&rule.kind), "{} is not in KINDS", rule.kind);
        }
        for extra in [
            "chat.colony",
            "loop.pause",
            "loop.resume",
            "workspace.enable",
            "workspace.disable",
            "colony.answer",
        ] {
            assert!(KINDS.contains(&extra), "{extra}");
        }
    }

    /// A router shaped like the real one: the handlers are stand-ins, the layers are the real ones.
    fn router(app: &Shared) -> Router {
        let stop_app = app.clone();
        let secret_app = app.clone();
        let launch_app = app.clone();
        Router::new()
            .route(
                "/api/sessions",
                post(move |body: String| {
                    let app = launch_app.clone();
                    async move {
                        let asked: Value = serde_json::from_str(&body).unwrap_or_default();
                        let mut made = colony("new1", SessionStatus::Queued);
                        made.origin = asked["origin"].as_str().map(str::to_string);
                        app.sessions.write().await.push(made.clone());
                        Json(made)
                    }
                }),
            )
            .route(
                "/api/sessions/{id}/stop",
                post(move |axum::extract::Path(id): axum::extract::Path<String>| {
                    let app = stop_app.clone();
                    async move {
                        app.update_session(&id, |s| s.status = SessionStatus::Stopped).await;
                        "stopped"
                    }
                }),
            )
            .route("/api/sessions/{id}/retain", post(|| async { "kept" }))
            .route(
                "/api/repos/{owner}/{name}/issues",
                post(|| async {
                    Json(serde_json::json!({"repo": "acme/web", "number": 77, "title": "Add dark mode", "url": "https://github.com/acme/web/issues/77"}))
                }),
            )
            .route(
                "/api/secrets/{id}",
                axum::routing::put(move |body: String| {
                    let _ = &secret_app;
                    async move {
                        if body.is_empty() {
                            StatusCode::BAD_REQUEST
                        } else {
                            StatusCode::OK
                        }
                    }
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(app.clone(), record_actions))
            .layer(axum::middleware::from_fn_with_state(app.clone(), crate::server::host_guard))
            .with_state(app.clone())
    }

    fn request(app: &Shared, method: &str, uri: &str, body: &str) -> Request {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:7878")
            .header("authorization", format!("Bearer {}", app.api_token))
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// One Stop press is one line: the handler's own transition takes the person as its actor, and
    /// the route layer writes nothing more for that colony.
    #[tokio::test]
    async fn a_stop_through_the_api_is_one_line_with_the_person_as_actor() {
        let root = root();
        let app = test_app(&root);
        app.sessions.write().await.push(colony("a", SessionStatus::Running));
        let res = router(&app)
            .oneshot(request(&app, "POST", "/api/sessions/a/stop", ""))
            .await
            .unwrap();
        assert!(res.status().is_success());
        let lines = all(&app);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(
            (lines[0].kind.as_str(), lines[0].actor.as_str(), lines[0].via.as_deref()),
            ("outcome.stopped", "you", Some("api"))
        );
        // Stopping it again changes no status: the press is recorded as the person's action.
        router(&app)
            .oneshot(request(&app, "POST", "/api/sessions/a/stop", ""))
            .await
            .unwrap();
        let lines = all(&app);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].kind, "colony.stop");
    }

    #[tokio::test]
    async fn a_secret_save_names_the_secret_never_its_value_and_a_refusal_writes_nothing() {
        let root = root();
        let app = test_app(&root);
        let value = "sk-live-do-not-log-me";
        router(&app)
            .oneshot(request(
                &app,
                "PUT",
                "/api/secrets/provider-keys%3Aopenrouter",
                &format!("{{\"value\":\"{value}\"}}"),
            ))
            .await
            .unwrap();
        router(&app)
            .oneshot(request(&app, "PUT", "/api/secrets/other", ""))
            .await
            .unwrap();
        let lines = all(&app);
        assert_eq!(lines.len(), 1, "a refused save is not recorded: {lines:?}");
        assert_eq!(lines[0].kind, "settings.save");
        assert_eq!(lines[0].target.as_deref(), Some("secret provider-keys:openrouter"));
        assert_eq!(lines[0].section.as_deref(), Some("secrets"));
        let raw = std::fs::read_to_string(root.join("data").join(FILE)).unwrap();
        assert!(!raw.contains(value), "{raw}");
    }

    #[tokio::test]
    async fn an_unrecorded_route_and_an_unknown_colony_write_nothing() {
        let root = root();
        let app = test_app(&root);
        // `retain` is recorded, but only for a colony that exists.
        router(&app)
            .oneshot(request(&app, "POST", "/api/sessions/nope/retain", ""))
            .await
            .unwrap();
        assert!(all(&app).is_empty());
        app.sessions.write().await.push(colony("b", SessionStatus::Stopped));
        router(&app)
            .oneshot(request(&app, "POST", "/api/sessions/b/retain", ""))
            .await
            .unwrap();
        let lines = all(&app);
        assert_eq!((lines.len(), lines[0].kind.as_str()), (1, "colony.retain"));
    }

    #[tokio::test]
    async fn a_launch_names_its_colony_and_a_chat_launch_says_so() {
        let root = root();
        let app = test_app(&root);
        router(&app)
            .oneshot(request(&app, "POST", "/api/sessions", "{}"))
            .await
            .unwrap();
        router(&app)
            .oneshot(request(&app, "POST", "/api/sessions", "{\"origin\":\"chat\"}"))
            .await
            .unwrap();
        let lines = all(&app);
        assert_eq!(
            lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
            vec!["colony.launch", "chat.colony"]
        );
        assert_eq!(lines[0].colony.as_deref(), Some("new1"));
        assert_eq!((lines[0].repo.as_deref(), lines[0].issue), (Some("acme/web"), Some(12)));
        assert_eq!(lines[0].title.as_deref(), Some("Fix the checkout"));
    }

    #[tokio::test]
    async fn colonize_records_the_issue_it_filed_and_the_colonies_it_sent() {
        let root = root();
        let app = test_app(&root);
        router(&app)
            .oneshot(request(
                &app,
                "POST",
                "/api/repos/acme/web/issues",
                "{\"title\":\"Add dark mode\"}",
            ))
            .await
            .unwrap();
        router(&app)
            .oneshot(request(&app, "POST", "/api/sessions", "{\"origin\":\"colonize\"}"))
            .await
            .unwrap();
        let lines = all(&app);
        assert_eq!(
            lines.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
            vec!["colonize.issue", "colonize.colony"]
        );
        assert_eq!(
            (lines[0].repo.as_deref(), lines[0].org.as_deref()),
            (Some("acme/web"), Some("acme"))
        );
        assert_eq!((lines[0].issue, lines[0].title.as_deref()), (Some(77), Some("Add dark mode")));
        assert_eq!(lines[1].colony.as_deref(), Some("new1"));
    }
}
