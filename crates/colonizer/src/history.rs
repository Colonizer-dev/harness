//! Cross-colony conversation history search (issue #739): the owner searches every colony's
//! `events.jsonl` from the cockpit, and a colony searches its neighbours' over the gateway. A hit is
//! one `user_message` or `assistant_text` whose text holds every whitespace-separated query term, with
//! a snippet around the first match. Scoping is the point: a colony sees only same-org neighbours
//! (same-repo and org-less ones when it has no org), never itself, and never a `restricted` — or
//! unclassified — log unless it is `restricted` itself. Every event is redacted on the way out.

use crate::sensitivity::Sensitivity;
use crate::sessions::{Session, SessionStatus};
use crate::{ApiResult, Shared, client_error};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};

/// Hits returned unless the request asks for fewer, and the most it can ask for.
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;
/// Hits one colony may contribute: a chatty colony must not fill the whole result.
const PER_COLONY: usize = 3;
/// Characters of context a snippet carries, centred on the first match.
const SNIPPET_CHARS: usize = 240;
/// Bytes read from one colony's log — its tail, so recent turns win — and across one whole request
/// before scanning stops; a line over `MAX_LINE_BYTES` is skipped rather than parsed.
const COLONY_BYTES: u64 = 8 * 1024 * 1024;
const REQUEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 256 * 1024;

/// One matching message, as the wire returns it.
#[derive(Debug, Serialize)]
struct Hit {
    colony: String,
    repo: String,
    org: String,
    agent: String,
    status: SessionStatus,
    created_at: DateTime<Utc>,
    seq: u64,
    ts: Option<String>,
    /// The `user_message.id` or `assistant_text.message_id` the message belongs to, when present.
    turn: Option<String>,
    role: &'static str,
    snippet: String,
}

/// What a scan pulls off a matching line; the colony fields are added per session.
struct RawHit {
    seq: u64,
    ts: Option<String>,
    turn: Option<String>,
    role: &'static str,
    snippet: String,
}

/// What the owner route narrows a search to; each unset field matches everything.
#[derive(Default)]
struct Filter {
    repo: Option<String>,
    org: Option<String>,
    agent: Option<String>,
    status: Option<String>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

impl Filter {
    fn matches(&self, s: &Session) -> bool {
        self.repo.as_deref().is_none_or(|v| s.repo == v)
            && self.org.as_deref().is_none_or(|v| s.org == v)
            && self.agent.as_deref().is_none_or(|v| s.agent == v)
            && self.status.as_deref().is_none_or(|v| s.status.as_str() == v)
            && self.since.is_none_or(|v| s.created_at >= v)
            && self.until.is_none_or(|v| s.created_at <= v)
    }
}

/// Whether `caller` may see `s`'s history — the security core. A colony never sees itself; one with an
/// org sees only same-org colonies and never crosses an org boundary, while one with no org sees only
/// org-less colonies of its own repository. A [`protected`] log is readable only by a `restricted` caller.
fn visible_to(caller: &Session, s: &Session) -> bool {
    if s.id == caller.id {
        return false;
    }
    let same_scope = if caller.org.is_empty() {
        s.org.is_empty() && s.repo == caller.repo
    } else {
        s.org == caller.org
    };
    same_scope && !(protected(s) && !restricted(caller))
}

/// Whether a log is protected: it parses `restricted`, or its sensitivity is missing or unparseable.
/// An unknown class fails closed, so nothing is read by accident.
fn protected(s: &Session) -> bool {
    !matches!(
        Sensitivity::parse(s.sensitivity.as_deref().unwrap_or_default()),
        Some(Sensitivity::Open | Sensitivity::Standard | Sensitivity::Custom | Sensitivity::Vetted)
    )
}

/// Whether a colony is explicitly `restricted`, the only caller that may read a protected log.
fn restricted(s: &Session) -> bool {
    Sensitivity::parse(s.sensitivity.as_deref().unwrap_or_default()) == Some(Sensitivity::Restricted)
}

/// The limit a request asks for, clamped to [`MAX_LIMIT`] with [`DEFAULT_LIMIT`] when unset.
fn limit_of(limit: Option<u32>) -> usize {
    limit.map_or(DEFAULT_LIMIT, |v| (v as usize).min(MAX_LIMIT))
}

/// An RFC 3339 instant, a naive `YYYY-MM-DDTHH:MM:SS` (read as UTC), or a bare `YYYY-MM-DD` day —
/// its start, or with `end_of_day` its end.
fn parse_when(raw: &str, end_of_day: bool) -> Option<DateTime<Utc>> {
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Some(at.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        return Some(naive.and_utc());
    }
    let day = NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok()?;
    let at = if end_of_day {
        day.and_hms_milli_opt(23, 59, 59, 999)?
    } else {
        day.and_hms_opt(0, 0, 0)?
    };
    Some(at.and_utc())
}

/// Runs a search over the colonies `visible` admits, newest first, up to `limit` hits and
/// [`PER_COLONY`] per colony, reading at most [`REQUEST_BYTES`] across the request. Each file is read
/// in a blocking task, so a large `events.jsonl` never stalls the async runtime.
async fn search(app: &Shared, query: &str, limit: usize, visible: impl Fn(&Session) -> bool) -> Vec<Hit> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut sessions: Vec<Session> = app.sessions.read().await.iter().filter(|s| visible(s)).cloned().collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    let mut hits = Vec::new();
    let mut budget = REQUEST_BYTES;
    for s in &sessions {
        if hits.len() >= limit || budget == 0 {
            break;
        }
        // The tail through the session store, whole lines only; the scan stays off the async runtime.
        let Ok(Some(tail)) = app.store().read_tail(&s.id, "events.jsonl", budget.min(COLONY_BYTES)).await else {
            continue;
        };
        let terms = terms.clone();
        let Ok((raw, read)) = tokio::task::spawn_blocking(move || scan_tail(&tail, &terms)).await else {
            continue;
        };
        budget = budget.saturating_sub(read);
        for r in raw {
            hits.push(Hit {
                colony: s.id.clone(),
                repo: s.repo.clone(),
                org: s.org.clone(),
                agent: s.agent.clone(),
                status: s.status,
                created_at: s.created_at,
                seq: r.seq,
                ts: r.ts,
                turn: r.turn,
                role: r.role,
                snippet: r.snippet,
            });
            if hits.len() >= limit {
                break;
            }
        }
    }
    hits
}

/// Scans the tail of a colony's `events.jsonl` (the store's last bytes within the budget, so a file
/// past the cap still contributes its recent turns), returning up to [`PER_COLONY`] matches and the
/// bytes read. A
/// line that will not parse, is not a `user_message`/`assistant_text`, or is over [`MAX_LINE_BYTES`]
/// is skipped; every event is redacted first.
fn scan_tail(tail: &[u8], terms: &[String]) -> (Vec<RawHit>, u64) {
    let mut reader = BufReader::new(tail);
    let mut line = String::new();
    let mut hits = Vec::new();
    while hits.len() < PER_COLONY {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF, or a read fault that ends the scan
            Ok(_) => {}
        }
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(mut event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        crate::redact::redact_value(&mut event); // a pre-#761 line can still hold a secret
        let role = match event.get("type").and_then(Value::as_str).unwrap_or_default() {
            "user_message" => "user",
            "assistant_text" => "assistant",
            _ => continue,
        };
        let Some(text) = event.get("text").and_then(Value::as_str) else {
            continue;
        };
        let lower = text.to_lowercase();
        if !terms.iter().all(|t| lower.contains(t.as_str())) {
            continue;
        }
        let at = terms
            .iter()
            .filter_map(|t| lower.find(t.as_str()))
            .min()
            .map_or(0, |at| map_offset(text, &lower, at));
        hits.push(RawHit {
            seq: event.get("seq").and_then(Value::as_u64).unwrap_or(0),
            ts: event.get("ts").and_then(Value::as_str).map(str::to_string),
            turn: event
                .get(if role == "user" { "id" } else { "message_id" })
                .and_then(Value::as_str)
                .map(str::to_string),
            role,
            snippet: snippet_around(text, at),
        });
    }
    (hits, tail.len() as u64)
}

/// Maps a byte offset in `lower` (a `to_lowercase` of `text`) back to one in `text`, walking the
/// original chars while accumulating each one's lowercased byte length. The two differ only for chars
/// whose lowercase form changes length (e.g. 'İ'), so copying the offset would split a character.
fn map_offset(text: &str, lower: &str, at: usize) -> usize {
    let mut seen = 0;
    for (i, c) in text.char_indices() {
        if seen >= at {
            return i;
        }
        seen += c.to_lowercase().map(char::len_utf8).sum::<usize>();
    }
    text.len().min(lower.len())
}

/// Up to [`SNIPPET_CHARS`] characters of `text` centred on the match at byte `at`, with an ellipsis on
/// whichever end was cut. Walks char boundaries, so a multi-byte character is never split.
fn snippet_around(text: &str, at: usize) -> String {
    let match_char = text[..at.min(text.len())].chars().count();
    let total = text.chars().count();
    let mut from = match_char.saturating_sub(SNIPPET_CHARS / 2);
    let to = (from + SNIPPET_CHARS).min(total);
    if to - from < SNIPPET_CHARS {
        from = to.saturating_sub(SNIPPET_CHARS);
    }
    let start = text.char_indices().nth(from).map_or(text.len(), |(i, _)| i);
    let end = text.char_indices().nth(to).map_or(text.len(), |(i, _)| i);
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    out.push_str(&text[start..end]);
    if to < total {
        out.push('…');
    }
    out
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
    repo: Option<String>,
    org: Option<String>,
    agent: Option<String>,
    status: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<u32>,
}

/// `GET /api/history/search?q=…`: owner-only search over every colony's log. An empty query or an
/// unparseable `since`/`until` is a 400. Cross-colony search is owner-level, so this route is
/// deliberately absent from `api_tokens::classify`'s allowlist.
async fn search_route(State(app): State<Shared>, Query(p): Query<SearchParams>) -> ApiResult<Value> {
    let q = p.q.as_deref().unwrap_or_default();
    if q.trim().is_empty() {
        return Err(client_error(StatusCode::BAD_REQUEST, "q must not be empty"));
    }
    let when = |v: Option<&str>, end: bool, name: &str| match v {
        Some(v) => parse_when(v, end).map(Some).ok_or_else(|| {
            client_error(
                StatusCode::BAD_REQUEST,
                &format!("{name} must be RFC3339, YYYY-MM-DDTHH:MM:SS or YYYY-MM-DD"),
            )
        }),
        None => Ok(None),
    };
    let filter = Filter {
        repo: p.repo,
        org: p.org,
        agent: p.agent,
        status: p.status,
        since: when(p.since.as_deref(), false, "since")?,
        until: when(p.until.as_deref(), true, "until")?,
    };
    Ok(Json(
        json!({"hits": search(&app, q, limit_of(p.limit), |s| filter.matches(s)).await}),
    ))
}

#[derive(Deserialize)]
pub(crate) struct HistoryBody {
    query: String,
    limit: Option<u32>,
}

/// `POST /history` on the colony gateway: a colony searches its neighbours' logs over its own gateway
/// token, scoped server-side by [`visible_to`]. A token that names no live colony is a 401; an empty
/// query reads as empty hits.
pub(crate) async fn history(State(app): State<Shared>, headers: HeaderMap, Json(body): Json<HistoryBody>) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let Some(caller) = app.colony_for_token(token).await else {
        return crate::gateway::api_error(StatusCode::UNAUTHORIZED, "authentication_error", "unknown colony token", None);
    };
    let hits = search(&app, &body.query, limit_of(body.limit), |s| visible_to(&caller, s)).await;
    Json(json!({"hits": hits})).into_response()
}

/// This module's feature descriptor (`features.rs`): its one owner route. No scoped-token rule on
/// purpose — a search across every colony is an owner-level read, so scoped tokens fall to the
/// legacy `classify` arms and are refused — and no activity rule, since a search changes nothing.
/// The colony-facing `POST /history` lives on the gateway router, not here.
pub(crate) const FEATURE: crate::features::Feature = crate::features::Feature {
    name: "history",
    routes,
    token_scope: None,
    activity: &[],
    kinds: &[],
    start_tasks: None,
};

fn routes() -> axum::Router<crate::Shared> {
    axum::Router::new().route("/api/history/search", routing::get(search_route))
}

#[cfg(test)]
mod tests;
