//! Colony-to-colony coordination (issue #834): file claims and short messages between the live
//! colonies of one repository, over the colony gateway (`POST /coordinate`, docs/protocol.md §6.6b).
//!
//! Parallel colonies have no channel to each other, so two can append to the same file and one pull
//! request conflicts the other. A colony claims the paths it is about to change, sees which live
//! same-repo colonies already hold or touch them, and can message them to agree who goes first.

use crate::gateway::bearer_token;
use crate::{ApiResult, App, AppError, Shared, client_error, sessions::Session};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How many paths one colony may hold claimed at once.
pub const MAX_CLAIMS: usize = 200;
/// How long a claim stays live without being refreshed.
pub const CLAIM_TTL_HOURS: i64 = 12;
/// How many inbox messages one read returns.
pub const INBOX_CAP: usize = 50;
/// The most one message may carry, after redaction.
pub const MAX_MESSAGE: usize = 4000;
/// The most one colony may send in a rolling hour, counted from `sent.jsonl`.
pub const SEND_MAX_PER_HOUR: usize = 20;
/// The shortest colony-id prefix that may name a recipient: any shorter is a typo, not an address.
const MIN_ID_PREFIX: usize = 4;
/// The reason a conflict path carries when it came from a colony's pull-request file list.
const DERIVED_REASON: &str = "changed in its pull request";
/// What a colony is told to do about a conflict.
const ADVICE: &str = "Another live colony in this repository already holds one of these paths. Either wait for its \
     pull request, coordinate with the send and inbox tools (agree who appends where, or who merges first), or \
     proceed keeping your edits in the shared file minimal and additive.";

/// A claimed path with why and when; `sessions/<id>/claims.json` is a list of these. A path derived
/// from a colony's pull request carries [`DERIVED_REASON`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub at: DateTime<Utc>,
}

/// One message line (`sessions/<id>/inbox.jsonl`; `at` is all a `sent.jsonl` line is read back for).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Message {
    #[serde(default)]
    from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from_issue: Option<u64>,
    #[serde(default)]
    text: String,
    at: DateTime<Utc>,
}

/// One overlapping colony in a `claim` reply.
#[derive(Serialize)]
struct Conflict {
    colony: String,
    issue: Option<u64>,
    paths: Vec<String>,
    reason: Option<String>,
}

/// A path as it is stored and compared: trimmed, no leading `./` or `/`, empty and `.` segments and
/// duplicate slashes collapsed, no trailing `/`. `None` for a path with a `..` segment: it can escape
/// the repository, so it is refused rather than normalised into something a peer's claim would miss.
fn normalize_path(raw: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in raw.trim().split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            keep => segments.push(keep),
        }
    }
    Some(segments.join("/"))
}

/// Whether two normalised paths overlap: the same file, or one a directory prefix of the other.
fn overlaps(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/")))
}

/// Whether a claim has aged out at `now`.
fn expired(at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(at) > Duration::hours(CLAIM_TTL_HOURS)
}

/// `colony <id> (#issue)`, for a log line a person reads.
fn label(id: &str, issue: Option<u64>) -> String {
    issue.map_or_else(|| format!("colony {id}"), |n| format!("colony {id} (#{n})"))
}

/// The live colonies of the caller's repository except the caller itself.
fn siblings<'a>(all: &'a [Session], caller: &Session) -> Vec<&'a Session> {
    all.iter()
        .filter(|s| s.id != caller.id && s.repo == caller.repo && s.status.is_live())
        .collect()
}

/// Reads a colony's claims ledger, tolerating a missing or torn file. The ledger is written
/// atomically under the colony's file lock, so a reader sees one whole version, never a half-write.
async fn read_claims(app: &App, id: &str) -> Vec<Claim> {
    app.store()
        .read_file(id, CLAIMS_FILE)
        .await
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// A colony's held-path claims, in the session store.
const CLAIMS_FILE: &str = "claims.json";
/// The messages a colony has received.
const INBOX_FILE: &str = "inbox.jsonl";
/// The messages a colony has sent, for the hourly rate limit.
const SENT_FILE: &str = "sent.jsonl";

/// The paths one colony holds: its non-expired claims, then its pull-request file list.
async fn held_paths(app: &App, s: &Session, now: DateTime<Utc>) -> Vec<Claim> {
    let mut out: Vec<Claim> = read_claims(app, &s.id)
        .await
        .into_iter()
        .filter(|c| !c.path.is_empty() && !expired(c.at, now))
        .collect();
    for raw in &s.changed_paths {
        let Some(path) = normalize_path(raw) else {
            continue;
        };
        if !path.is_empty() && !out.iter().any(|c| c.path == path) {
            out.push(Claim {
                path,
                reason: Some(DERIVED_REASON.into()),
                at: now,
            });
        }
    }
    out
}

/// Every live same-repo colony, the caller's own included when `include_self`, with the paths it
/// holds.
async fn holdings<'a>(app: &App, all: &'a [Session], caller: &Session, include_self: bool) -> Vec<(&'a Session, Vec<Claim>)> {
    let now = Utc::now();
    let mut out = Vec::new();
    for s in all
        .iter()
        .filter(|s| s.repo == caller.repo && s.status.is_live() && (include_self || s.id != caller.id))
    {
        out.push((s, held_paths(app, s, now).await));
    }
    out
}

/// The conflicts between `mine` and the peers' held paths: one entry per peer. Pure, so the rule is
/// tested without a server.
fn conflicts(mine: &[String], held: &[(&Session, Vec<Claim>)]) -> Vec<Conflict> {
    held.iter()
        .filter_map(|(s, paths)| {
            let hit: Vec<&Claim> = paths.iter().filter(|p| mine.iter().any(|m| overlaps(m, &p.path))).collect();
            (!hit.is_empty()).then(|| Conflict {
                colony: s.id.clone(),
                issue: s.issue,
                reason: hit.iter().find_map(|p| p.reason.clone()),
                paths: hit.iter().map(|p| p.path.clone()).collect(),
            })
        })
        .collect()
}

/// Resolves `to` — a colony id, an id prefix, or an issue number (`831` or `#831`) — among the
/// caller's live same-repo peers, or an error naming what went wrong.
fn recipient<'a>(peers: &[&'a Session], to: &str, repo: &str) -> Result<&'a Session, AppError> {
    if let Some(exact) = peers.iter().find(|s| s.id == to) {
        return Ok(exact);
    }
    let matched: Vec<&Session> = match to.trim_start_matches('#').parse::<u64>() {
        Ok(issue) => peers.iter().filter(|s| s.issue == Some(issue)).copied().collect(),
        Err(_) if to.chars().count() >= MIN_ID_PREFIX => peers.iter().filter(|s| s.id.starts_with(to)).copied().collect(),
        Err(_) => Vec::new(),
    };
    match matched.as_slice() {
        [only] => Ok(only),
        [] => Err(client_error(
            StatusCode::NOT_FOUND,
            &format!("no live colony in {repo} matches \"{to}\""),
        )),
        many => Err(client_error(
            StatusCode::CONFLICT,
            &format!(
                "\"{to}\" matches more than one colony: {}",
                many.iter().map(|s| s.id.clone()).collect::<Vec<_>>().join(", ")
            ),
        )),
    }
}

/// How many messages a colony sent in the hour ending at `now`, read from its `sent.jsonl`.
async fn sends_last_hour(app: &App, id: &str, now: DateTime<Utc>) -> usize {
    let Ok(Some(bytes)) = app.store().read_file(id, SENT_FILE).await else {
        return 0;
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| serde_json::from_str::<Message>(l).ok())
        .filter(|m| now.signed_duration_since(m.at) <= Duration::hours(1))
        .count()
}

// ── The gateway handler ──

/// `POST /coordinate` on the colony gateway: the four ops, over the token's own colony. Authenticated
/// exactly like `recall` — a token that does not name a live colony is a 401.
pub(crate) async fn coordinate(State(app): State<Shared>, headers: HeaderMap, Json(body): Json<Value>) -> ApiResult<Value> {
    let Some(caller) = app.colony_for_token(bearer_token(&headers).unwrap_or_default()).await else {
        return Err(client_error(StatusCode::UNAUTHORIZED, "unknown colony token"));
    };
    match body.get("op").and_then(Value::as_str).unwrap_or_default() {
        "claim" => claim(&app, &caller, &body).await,
        "claims" => claims(&app, &caller).await,
        "send" => send(&app, &caller, &body).await,
        "inbox" => inbox(&app, &caller).await,
        other => Err(client_error(
            StatusCode::BAD_REQUEST,
            &format!("unknown op \"{other}\": one of claim, claims, send, inbox"),
        )),
    }
}

/// `claim`: record the caller's paths and answer what they collide with.
async fn claim(app: &App, caller: &Session, body: &Value) -> ApiResult<Value> {
    let Some(raw) = body.get("paths").and_then(Value::as_array) else {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "claim needs a \"paths\" array of strings",
        ));
    };
    let reason = body
        .get("reason")
        .and_then(Value::as_str)
        .map(|r| crate::redact::redact_text(r.trim()).into_owned())
        .filter(|r| !r.is_empty());
    let mut claimed: Vec<String> = Vec::new();
    for value in raw {
        let Some(text) = value.as_str() else {
            return Err(client_error(StatusCode::BAD_REQUEST, "every path must be a string"));
        };
        let Some(path) = normalize_path(text) else {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                &format!("path \"{text}\" must not contain a \"..\" segment"),
            ));
        };
        if !path.is_empty() && !claimed.contains(&path) {
            claimed.push(path);
        }
    }
    if claimed.is_empty() {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "claim needs at least one non-empty path",
        ));
    }

    // Merge into the ledger under the colony's file lock, so concurrent claims from one colony do not
    // lose an update. A re-claim refreshes its reason and time; expired entries, and the caller's own
    // earlier ones, are dropped first, then the newest within the cap kept, so the file cannot grow
    // without bound. The write is atomic, so a peer reading it never sees a truncated or empty list.
    let now = Utc::now();
    let rt = app.runtime(&caller.id).await;
    let claims = {
        let _guard = rt.file_lock.lock().await;
        let mut claims: Vec<Claim> = read_claims(app, &caller.id)
            .await
            .into_iter()
            .filter(|c| !c.path.is_empty() && !expired(c.at, now) && !claimed.contains(&c.path))
            .collect();
        claims.extend(claimed.iter().map(|path| Claim {
            path: path.clone(),
            reason: reason.clone(),
            at: now,
        }));
        claims.sort_by_key(|c| c.at);
        if claims.len() > MAX_CLAIMS {
            claims.drain(0..claims.len() - MAX_CLAIMS);
        }
        // Best effort, as before: a claim that cannot be saved is still echoed, and the next one
        // rewrites the whole list.
        if let Ok(bytes) = serde_json::to_vec_pretty(&claims) {
            let _ = app.store().write_file(&caller.id, CLAIMS_FILE, &bytes).await;
        }
        claims
    };
    // Echo only the paths the ledger kept, so one the cap pushed out is not reported as claimed.
    claimed.retain(|p| claims.iter().any(|c| c.path == *p));

    let all = app.sessions.read().await.clone();
    let held = holdings(app, &all, caller, false).await;
    let found = conflicts(&claimed, &held);

    // Both sides learn about a collision: the caller why it should hold off, the other colony that a
    // peer is about to touch its paths. Each line names the other colony and its issue.
    for c in &found {
        let why = c.reason.as_deref().unwrap_or("held by that colony");
        let paths = c.paths.join(", ");
        app.session_log(
            &caller.id,
            "warn",
            format!("claim on {paths} conflicts with {}: {why}", label(&c.colony, c.issue)),
        )
        .await;
        app.session_log(
            &c.colony,
            "info",
            format!(
                "{} claimed paths this colony is changing: {paths}",
                label(&caller.id, caller.issue)
            ),
        )
        .await;
    }

    let mut out = json!({
        "ok": true,
        "colony": caller.id,
        "claimed": claimed,
        "total_claims": claims.len(),
        "conflicts": found,
    });
    if !found.is_empty() {
        out["advice"] = json!(ADVICE);
    }
    Ok(Json(out))
}

/// `claims`: every live same-repo colony's paths, explicit and derived, the caller's included.
async fn claims(app: &App, caller: &Session) -> ApiResult<Value> {
    let all = app.sessions.read().await.clone();
    let colonies: Vec<Value> = holdings(app, &all, caller, true)
        .await
        .into_iter()
        .map(|(s, paths)| json!({"colony": s.id, "issue": s.issue, "self": s.id == caller.id, "paths": paths}))
        .collect();
    Ok(Json(json!({"ok": true, "repo": caller.repo, "colonies": colonies})))
}

/// `send`: a short redacted message to one peer, rate-limited and recorded on both sides.
async fn send(app: &App, caller: &Session, body: &Value) -> ApiResult<Value> {
    let to = body.get("to").and_then(Value::as_str).unwrap_or("").trim();
    if to.is_empty() {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "send needs a \"to\": a colony id, its prefix, or an issue number like \"831\" or \"#831\"",
        ));
    }
    let text = crate::redact::redact_text(body.get("text").and_then(Value::as_str).unwrap_or("").trim()).into_owned();
    if text.is_empty() {
        return Err(client_error(StatusCode::BAD_REQUEST, "send needs non-empty \"text\""));
    }
    let first = crate::util::truncate(text.lines().next().unwrap_or("").trim(), 120);
    let text = crate::util::truncate(&text, MAX_MESSAGE);

    let now = Utc::now();
    let recent = sends_last_hour(app, &caller.id, now).await;
    if recent >= SEND_MAX_PER_HOUR {
        return Err(client_error(
            StatusCode::TOO_MANY_REQUESTS,
            &format!("rate limited: at most {SEND_MAX_PER_HOUR} messages per colony per hour"),
        ));
    }

    let all = app.sessions.read().await.clone();
    let peers = siblings(&all, caller);
    let peer = recipient(&peers, to, &caller.repo)?.clone();

    let delivered = Message {
        from: caller.id.clone(),
        from_issue: caller.issue,
        text: text.clone(),
        at: now,
    };
    let line = crate::redact::redact_line(&serde_json::to_string(&delivered).unwrap_or_default()).into_owned();
    app.store().append(&peer.id, INBOX_FILE, line.as_bytes()).await.map_err(|e| {
        client_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("could not deliver the message: {e:#}"),
        )
    })?;
    let recorded = json!({"to": peer.id, "to_issue": peer.issue, "text": text, "at": now});
    let line = crate::redact::redact_line(&recorded.to_string()).into_owned();
    if let Err(e) = app.store().append(&caller.id, SENT_FILE, line.as_bytes()).await {
        app.storage_failed("append to the sent-messages log", &anyhow::Error::from(e))
            .await;
    }

    app.session_log(
        &caller.id,
        "info",
        format!("sent message to {}: {first}", label(&peer.id, peer.issue)),
    )
    .await;
    app.session_log(
        &peer.id,
        "info",
        format!("message from {}: {first}", label(&caller.id, caller.issue)),
    )
    .await;

    Ok(Json(json!({
        "ok": true,
        "to": peer.id,
        "to_issue": peer.issue,
        "at": now,
        "remaining_this_hour": SEND_MAX_PER_HOUR - recent - 1,
    })))
}

/// `inbox`: the caller's messages, newest first.
async fn inbox(app: &App, caller: &Session) -> ApiResult<Value> {
    let mut messages: Vec<Message> = match app.store().read_file(&caller.id, INBOX_FILE).await {
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        _ => Vec::new(),
    };
    messages.sort_by_key(|m| std::cmp::Reverse(m.at));
    messages.truncate(INBOX_CAP);
    Ok(Json(
        json!({"ok": true, "colony": caller.id, "count": messages.len(), "messages": messages}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{Session, SessionStatus};
    use std::path::{Path, PathBuf};

    /// A colony of `repo` (owner/name) with a real id and issue, as every op keys on all three.
    fn colony_with(id: &str, repo: &str, issue: u64, status: SessionStatus) -> Session {
        let mut s = crate::sessions::tests::colony(repo.split('/').next().unwrap_or("acme"), status);
        s.id = id.into();
        s.repo = repo.into();
        s.issue = Some(issue);
        s
    }

    /// A fresh app whose live sessions are exactly `colonies`, and its temp root.
    async fn app_with(colonies: Vec<Session>) -> (Shared, PathBuf) {
        let root = std::env::temp_dir().join(format!("colonizer-coordination-{}", crate::util::short_id()));
        let app = crate::tests::test_app(&root);
        *app.sessions.write().await = colonies;
        (app, root)
    }

    fn harness_log(app: &App, id: &str) -> String {
        std::fs::read_to_string(app.session_dir(id).join("harness.jsonl")).unwrap_or_default()
    }

    fn done(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
    }

    // Each op is the handler above, unwrapped to the reply (or the error) a test asserts on.
    async fn claim(app: &App, caller: &Session, body: Value) -> Value {
        super::claim(app, caller, &body).await.unwrap().0
    }
    async fn claims(app: &App, caller: &Session) -> Value {
        super::claims(app, caller).await.unwrap().0
    }
    async fn inbox(app: &App, caller: &Session) -> Value {
        super::inbox(app, caller).await.unwrap().0
    }
    async fn send(app: &App, caller: &Session, body: Value) -> Result<Value, AppError> {
        super::send(app, caller, &body).await.map(|json| json.0)
    }

    #[tokio::test]
    async fn a_claim_reports_every_kind_of_collision_and_logs_both_timelines() {
        // a is mid-pull-request on server.rs; b is a second live colony of the same repository.
        let mut a = colony_with("aaaaaaaa", "acme/app", 7, SessionStatus::Running);
        a.changed_paths = vec!["crates/colonizer/src/server.rs".into()];
        let b = colony_with("bbbbbbbb", "acme/app", 9, SessionStatus::Running);
        let (app, root) = app_with(vec![a.clone(), b.clone()]).await;

        // A path nobody holds is free.
        let free = claim(&app, &a, json!({"paths": ["src/a.rs"], "reason": "the page"})).await;
        assert_eq!(free["conflicts"].as_array().unwrap().len(), 0, "{free:?}");

        // b claiming a's changed directory (with `./` noise) collides by prefix, on a's derived reason.
        let out = claim(
            &app,
            &b,
            json!({"paths": ["./crates/colonizer/src"], "reason": "adding a route"}),
        )
        .await;
        let conflict = &out["conflicts"].as_array().unwrap()[0];
        assert_eq!(conflict["colony"].as_str(), Some("aaaaaaaa"), "{out:?}");
        assert_eq!(conflict["issue"].as_u64(), Some(7));
        assert_eq!(conflict["reason"].as_str(), Some(DERIVED_REASON));
        assert!(
            conflict["paths"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p.as_str() == Some("crates/colonizer/src/server.rs"))
        );
        assert!(out["advice"].as_str().unwrap().contains("wait"), "{out:?}");

        // a claiming b's explicitly-held directory collides the other way, carrying b's own reason.
        let back = claim(&app, &a, json!({"paths": ["crates/colonizer/src/server.rs"]})).await;
        assert_eq!(
            back["conflicts"].as_array().unwrap()[0]["reason"].as_str(),
            Some("adding a route"),
            "{back:?}"
        );

        // Both timelines name the other colony and its issue.
        let b_log = harness_log(&app, "bbbbbbbb");
        assert!(
            b_log.contains("colony aaaaaaaa (#7)") && b_log.contains("claimed paths this colony"),
            "{b_log}"
        );
        let a_log = harness_log(&app, "aaaaaaaa");
        assert!(
            a_log.contains("colony bbbbbbbb (#9)") && a_log.contains("claimed paths this colony"),
            "{a_log}"
        );

        // `claims` lists every colony of the repository, the caller's own included.
        let listed = claims(&app, &a).await;
        let ids: Vec<&str> = listed["colonies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["colony"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"aaaaaaaa") && ids.contains(&"bbbbbbbb"), "{listed:?}");
        done(&root);
    }

    #[tokio::test]
    async fn another_repository_or_a_stopped_colony_does_not_conflict() {
        let a = colony_with("aaaaaaaa", "acme/app", 7, SessionStatus::Running);
        let b = colony_with("bbbbbbbb", "acme/other", 9, SessionStatus::Running);
        let (app, root) = app_with(vec![a.clone(), b.clone()]).await;
        let _ = claim(&app, &a, json!({"paths": ["src/lib.rs"]})).await;
        let out = claim(&app, &b, json!({"paths": ["src/lib.rs"]})).await;
        assert_eq!(
            out["conflicts"].as_array().unwrap().len(),
            0,
            "repositories do not share paths: {out:?}"
        );
        done(&root);

        // A stopped colony is not live, so even its pull-request paths do not count.
        let mut stopped = colony_with("aaaaaaaa", "acme/app", 7, SessionStatus::Stopped);
        stopped.changed_paths = vec!["crates/colonizer/src/server.rs".into()];
        let b = colony_with("bbbbbbbb", "acme/app", 9, SessionStatus::Running);
        let (app, root) = app_with(vec![stopped, b.clone()]).await;
        let out = claim(&app, &b, json!({"paths": ["crates/colonizer/src/server.rs"]})).await;
        assert_eq!(
            out["conflicts"].as_array().unwrap().len(),
            0,
            "a stopped colony is not live: {out:?}"
        );
        done(&root);
    }

    #[tokio::test]
    async fn send_redacts_is_rate_limited_and_reaches_the_inbox() {
        let a = colony_with("aaaaaaaa", "acme/app", 7, SessionStatus::Running);
        let b = colony_with("bbbbbbbb", "acme/app", 9, SessionStatus::Running);
        let (app, root) = app_with(vec![a.clone(), b.clone()]).await;

        // A secret the redact module's own tests use; the recipient is named by issue number.
        let secret = "ghp_aB3dE5gH7jK9mN1pQ3sT5vX7zA9cE1gH3jK5";
        let out = send(&app, &a, json!({"to": "#9", "text": format!("the key is {secret}")}))
            .await
            .unwrap();
        assert_eq!(out["to"].as_str(), Some("bbbbbbbb"), "{out:?}");
        let stored = std::fs::read_to_string(app.session_dir("bbbbbbbb").join("inbox.jsonl")).unwrap();
        assert!(stored.contains("[REDACTED:") && !stored.contains(secret), "{stored}");

        // A name that matches no live peer is refused, not delivered.
        for to in ["999", "nobody"] {
            let err = send(&app, &a, json!({"to": to, "text": "anyone there?"})).await.unwrap_err();
            assert_eq!(err.status(), StatusCode::NOT_FOUND, "{to}");
        }

        // Up to the cap is fine; the send past it is a 429.
        for i in 0..SEND_MAX_PER_HOUR - 1 {
            let _ = send(&app, &a, json!({"to": "bbbbbbbb", "text": format!("note {i}")}))
                .await
                .unwrap();
        }
        let err = send(&app, &a, json!({"to": "bbbbbbbb", "text": "one too many"}))
            .await
            .unwrap_err();
        assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);

        // The inbox reads every message back, naming the sender.
        let read = inbox(&app, &b).await;
        let messages = read["messages"].as_array().unwrap();
        assert_eq!(messages.len(), SEND_MAX_PER_HOUR, "{read:?}");
        assert!(messages.iter().any(|m| m["text"].as_str() == Some("note 18")), "{read:?}");
        assert!(messages.iter().all(|m| m["from"].as_str() == Some("aaaaaaaa")), "{read:?}");
        done(&root);
    }

    #[test]
    fn paths_normalise_and_overlap_only_on_a_shared_file_or_directory_prefix() {
        assert_eq!(normalize_path("  ./crates/x.rs/  ").as_deref(), Some("crates/x.rs"));
        assert_eq!(normalize_path("/crates/x.rs").as_deref(), Some("crates/x.rs"));
        // Interior `./` and duplicate slashes collapse to one form, so they cannot mask a collision.
        assert_eq!(normalize_path("./crates//x.rs").as_deref(), Some("crates/x.rs"));
        // A `..` segment is refused: `src/a/../server.rs` must not slip past a claim on `src/server.rs`.
        assert_eq!(normalize_path("src/a/../server.rs"), None);
        assert!(overlaps("a/b.rs", "a/b.rs") && overlaps("a/b.rs", "a") && overlaps("a", "a/b.rs"));
        assert!(!overlaps("a/b.rs", "a/c.rs") && !overlaps("ab", "a/b") && !overlaps("", "a"));
    }
}
