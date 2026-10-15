//! The session HTTP handlers: list, detail, the open question and its answer, and the events and
//! terminal WebSockets.

use super::*;
use axum::http::HeaderMap;
use std::{collections::VecDeque, sync::LazyLock};

/// The query `GET /api/sessions` takes (issue #651): with neither field the route answers the
/// cockpit's bare array as it always has; with `limit` or `cursor` it answers a page. Both stay
/// strings here so a malformed value is this route's own **400** `invalid_input`, not the
/// extractor's plain-text rejection.
#[derive(Deserialize, Default)]
pub(crate) struct ListQuery {
    limit: Option<String>,
    cursor: Option<String>,
    /// The caller's own name for a colony (issue #901), matched exactly: `GET /api/sessions
    /// ?external_ref=<ref>`. Absent, the query says nothing about it.
    external_ref: Option<String>,
    /// The launcher tag a colony was created with (`Some("burn_down")`, `Some("redteam")`, …),
    /// matched exactly: `GET /api/sessions ?origin=<tag>`. Absent, the query says nothing about it.
    origin: Option<String>,
}

impl ListQuery {
    /// Whether the query asks for a page at all: either field present changes the reply shape.
    fn paginated(&self) -> bool {
        self.limit.is_some() || self.cursor.is_some()
    }

    /// The two exact-match filters as `(field, value)` pairs, or `Err` naming the field that named
    /// nothing: a filter sent blank is a **400**, never a filter that matches every colony (the
    /// shape a client gets for a bad `cursor`, §7.7). Values are trimmed, so `?origin=%20redteam%20`
    /// filters the way `?origin=redteam` does.
    fn filters(&self) -> Result<Vec<(&'static str, &str)>, &'static str> {
        let mut out = Vec::new();
        for (field, value) in [
            ("external_ref", self.external_ref.as_deref()),
            ("origin", self.origin.as_deref()),
        ] {
            let Some(raw) = value else { continue };
            let value = raw.trim();
            if value.is_empty() {
                return Err(field);
            }
            out.push((field, value));
        }
        Ok(out)
    }
}

/// The page size `GET /uhp/v1/sessions` always uses and `GET /api/sessions` defaults to (§7,
/// sessions: a client must not have to guess the end of the list from a short page).
const DEFAULT_PAGE_LIMIT: usize = 20;
const MAX_PAGE_LIMIT: usize = 100;

/// One page of the colony list: the page itself, newest first, and the cursor a client sends
/// back for the next one — the last session's id, or none when the list is exhausted.
pub(crate) struct SessionPage {
    sessions: Vec<Session>,
    next_cursor: Option<String>,
}

/// Slices one page off the visible list. The cursor names a colony the caller can already see,
/// and the page starts right after it; a cursor outside the visible list is refused, so ids
/// cannot be probed through it.
fn paginate(visible: Vec<Session>, limit: usize, cursor: Option<&str>) -> Result<SessionPage, ()> {
    let start = match cursor {
        None => 0,
        Some(cursor) => visible.iter().position(|s| s.id == cursor).ok_or(())? + 1,
    };
    let end = (start + limit).min(visible.len());
    let more = end < visible.len();
    // `more` means at least one session sits past `end`, so the page is not empty and its last
    // member is the cursor.
    let next_cursor = more.then(|| visible[end - 1].id.clone());
    let sessions = visible.into_iter().skip(start).take(end - start).collect();
    Ok(SessionPage { sessions, next_cursor })
}

/// The colonies a caller may see, newest first: the store's order reversed, with a scoped
/// token's org/repo limits applied as a filter that hides what it does not cover (issue #508) —
/// a colony outside them is not in the answer at all, the same hiding a single-colony read gets.
pub(crate) async fn visible_sessions(
    app: &App,
    scoped: Option<&axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Vec<Session> {
    let sessions = app.sessions.read().await.clone();
    let mut out = Vec::with_capacity(sessions.len());
    for session in sessions.into_iter().rev() {
        if let Some(token) = scoped
            && !token.covers(&session.org, &session.repo)
        {
            continue;
        }
        out.push(session);
    }
    out
}

/// The colony an `Idempotency-Key` already created for this caller (issue #901): looked up through
/// the same [`visible_sessions`] rule the list answers with, so a key can only ever hand back a
/// colony the caller could already have read — a key that names one outside a scoped token's
/// org/repo limits finds nothing, and the launch goes on as an ordinary new one. Newest first, as
/// the list is: a key reused long after its colony was removed finds nothing here and starts a
/// fresh colony.
pub(crate) async fn session_by_idempotency_key(
    app: &App,
    scoped: Option<&axum::Extension<crate::api_tokens::ScopedToken>>,
    key: &str,
) -> Option<Session> {
    visible_sessions(app, scoped)
        .await
        .into_iter()
        .find(|session| session.idempotency_key.as_deref() == Some(key))
}

/// The colony list the cockpit's sockets read beside the HTTP one (`stream.rs`) and
/// `diagnosis` builds on: the same newest-first, visibility-filtered `Session` array
/// `GET /api/sessions` answers without pagination params, decorated, as the hub has always
/// unwrapped it.
pub(crate) async fn list_bare(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Json<Vec<Session>> {
    let visible = visible_sessions(&app, scoped.as_ref()).await;
    Json(decorated(&app, visible).await)
}

/// The list narrowed by the query's `external_ref` / `origin` filters (issue #901), on top of the
/// visibility filter rather than around it: a colony a scoped token may not read stays out of the
/// answer whether or not it matches the filter. Matching is exact on the stored value, so a filter
/// naming nothing finds nothing — an empty list, not an error.
pub(crate) fn filtered(visible: Vec<Session>, query: &ListQuery) -> Result<Vec<Session>, &'static str> {
    let filters = query.filters()?;
    if filters.is_empty() {
        return Ok(visible);
    }
    Ok(visible
        .into_iter()
        .filter(|session| {
            filters.iter().all(|(field, value)| match *field {
                "external_ref" => session.external_ref.as_deref() == Some(value),
                _ => session.origin.as_deref() == Some(value),
            })
        })
        .collect())
}

/// The visible list with each colony's live activity attached — the one decoration loop, so the
/// hub's bare array and every page's `sessions` member answer the same bytes.
async fn decorated(app: &App, visible: Vec<Session>) -> Vec<Session> {
    let mut out = Vec::with_capacity(visible.len());
    for session in visible {
        out.push(with_activity(app, session).await);
    }
    out
}

/// The paginated colony list both surfaces answer with: `GET /api/sessions` when its query asks
/// for a page, `GET /uhp/v1/sessions` always (§7). `next_cursor` is the id of the page's last
/// session while more follow, so walking pages ends at `null` — the marker §7 sessions wants
/// instead of a client guessing the end from a short page.
pub(crate) async fn paged_list(app: &App, visible: Vec<Session>, query: &ListQuery, uhp: bool, headers: &HeaderMap) -> Response {
    let limit = match query.limit.as_deref() {
        None => DEFAULT_PAGE_LIMIT,
        Some(raw) => match raw.parse::<usize>() {
            Ok(parsed) => parsed.clamp(1, MAX_PAGE_LIMIT),
            Err(_) => {
                return wrong_input(
                    uhp,
                    headers,
                    format!("`limit` must be an integer between 1 and {MAX_PAGE_LIMIT}"),
                );
            }
        },
    };
    let page = match paginate(visible, limit, query.cursor.as_deref()) {
        Ok(page) => page,
        Err(()) => {
            return wrong_input(
                uhp,
                headers,
                "`cursor` names no colony in this list; read a page and send back its `next_cursor`",
            );
        }
    };
    let sessions = decorated(app, page.sessions).await;
    Json(json!({"sessions": sessions, "next_cursor": page.next_cursor})).into_response()
}

/// The **400** `invalid_input` a malformed pagination query answers (§7.7): envelope when the
/// request speaks UHP, Colonizer's string error with the code as a sibling otherwise.
pub(crate) fn wrong_input(uhp: bool, headers: &HeaderMap, message: impl std::fmt::Display) -> Response {
    crate::uhp::error_for(uhp, headers, StatusCode::BAD_REQUEST, "invalid_input", message, None)
}

/// `GET /api/sessions`: the cockpit's bare array, newest first, unless the query asks for a page
/// (`limit`/`cursor`, issue #651) — then `{"sessions": […], "next_cursor": …}`, the §7 shape,
/// with the page's items alone decorated with their live activity. `external_ref` and `origin`
/// (issue #901) narrow either shape; only `limit`/`cursor` change it.
pub async fn list(
    State(app): State<Shared>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Response {
    let visible = match filtered(visible_sessions(&app, scoped.as_ref()).await, &query) {
        Ok(visible) => visible,
        Err(field) => {
            return wrong_input(
                false,
                &headers,
                format!("`{field}` names nothing; send the value it filters on, or leave it out"),
            );
        }
    };
    if !query.paginated() {
        return Json(decorated(&app, visible).await).into_response();
    }
    paged_list(&app, visible, &query, false, &headers).await
}

pub async fn get(State(app): State<Shared>, Path(id): Path<String>) -> ApiResult<diagnosis::SessionDetail> {
    let session = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    Ok(Json(diagnosis::for_session(&app, with_activity(&app, session).await).await))
}

/// `GET /api/sessions/{id}/question` (issue #508): the question the colony's agent is waiting on,
/// if it is waiting on one — the same state the cockpit's choice cards read off the events socket,
/// so an external client sees exactly what a browser sees. `question_id` is what an answer names,
/// `questions` are the agent's own question bodies with their options, and `risk` is the question's
/// class, which bounds what answering it may unleash. A colony that is not asking reads **204** with
/// no body — an empty inbox, not an error — while an unknown id stays a 404.
pub async fn question(State(app): State<Shared>, Path(id): Path<String>) -> Result<Response, crate::AppError> {
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let rt = app.runtime(&id).await;
    let Some((question_id, questions, risk)) = rt.open_question().await else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    Ok(Json(json!({
        "question_id": question_id,
        "risk": risk.as_str(),
        "questions": questions,
    }))
    .into_response())
}

/// `POST /api/sessions/{id}/answer` (issue #508): the HTTP twin of the events socket's `answer`
/// command — same body the cockpit sends over the wire, the same shared path (`submit_answer`), the
/// same activity line. An answer from a scoped token carries the external-input marker (`forward`),
/// so the agent reads it as a description from outside, not as the operator's voice. Answers a
/// 409/404, never a silent drop: an HTTP caller cannot see the transcript, so a refusal must say
/// itself.
pub async fn answer(
    State(app): State<Shared>,
    Path(id): Path<String>,
    via: Option<axum::Extension<crate::auth::Via>>,
    Json(command): Json<Value>,
) -> Result<StatusCode, crate::AppError> {
    let via = via.map(|axum::Extension(via)| via);
    let Some(parsed) = AnswerCommand::parse(&command) else {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "expected {\"question_id\": str, \"answers\": {option: choice}, \"response\": str?} matching the open question",
        ));
    };
    // The name, when the answerer is a scoped token: it marks the free-text note the agent reads.
    let external_name = match &via {
        Some(crate::auth::Via::Token(name)) => Some(name.clone()),
        _ => None,
    };
    let external = external_name.as_deref();
    let rt = app.runtime(&id).await;
    match submit_answer(&app, &id, &rt, parsed, via, external, true).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(AnswerError::NoSession) => Err(client_error(StatusCode::NOT_FOUND, "no such session")),
        Err(AnswerError::NotAccepting(status)) => Err(client_error(
            StatusCode::CONFLICT,
            &format!("the colony is {} and cannot take an answer; resume it first", status.as_str()),
        )),
        Err(AnswerError::NoQuestion) => Err(client_error(StatusCode::CONFLICT, "no question is pending for this colony")),
        Err(AnswerError::Stale) => Err(client_error(
            StatusCode::CONFLICT,
            "the colony is asking a different question now; re-read GET /api/sessions/{id}/question and answer that one",
        )),
    }
}

/// `POST /api/sessions/{id}/prewarm` (issue #701): the caller has a suspended colony's question
/// open, so the queue may bring the colony back for the answer ahead of its timeout, and the answer
/// lands in an already-running VM. Marking the record is all the route does — the queue applies the
/// admission, the slot rules and the timeout on its own ticks — so a colony that is not a suspended
/// one still waiting on its question (live, already holding an answer) reads **204**: a no-op, not
/// an error. A marked or already-marked colony reads **202**; the queue never takes a slot ahead of
/// a colony that holds an answer.
pub async fn prewarm(
    State(app): State<Shared>,
    Path(id): Path<String>,
    via: Option<axum::Extension<crate::auth::Via>>,
) -> Result<StatusCode, crate::AppError> {
    let Some(s) = app.session(&id).await else {
        return Err(client_error(StatusCode::NOT_FOUND, "no such session"));
    };
    // Issue #673: a colony a merge superseded is held until kept, so there is nothing to warm.
    if !suspended_waiting(&s) || s.pending_answer.is_some() || crate::supersede::blocks_start(&s) {
        return Ok(StatusCode::NO_CONTENT);
    }
    // Conditional on purpose: a restore or a stop that claimed the colony between the snapshot and
    // this write must not hang a warm-up request on it. `false` here is an already-marked colony —
    // idempotent, so still a 202.
    let marked = app
        .update_session(&id, |x| {
            if !suspended_waiting(x) || x.pending_answer.is_some() || x.prewarm.is_some() || crate::supersede::blocks_start(x) {
                return false;
            }
            x.prewarm = Some(Prewarm {
                requested_at: Utc::now(),
                started_at: None,
                ready_at: None,
            });
            x.updated_at = Utc::now();
            true
        })
        .await
        .is_some_and(|(_, landed)| landed);
    if marked {
        app.session_log(
            &id,
            "info",
            "question opened; the queue brings the colony back when a slot frees".into(),
        )
        .await;
        if let Some(s) = app.session(&id).await {
            crate::activity::record_prewarm(&app, &s, via.map(|axum::Extension(via)| via)).await;
        }
    }
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/sessions/{id}/seen` (issue #744): someone is looking at this colony now, so a
/// failure it moved into is no longer unseen — the flag the app badge counts is cleared here, and
/// the silent `resolved` push lets every other device close its notification and lower the badge.
/// The push only fires when something was actually unseen: a look at a colony whose question is
/// still open must not close that question's notification elsewhere. Never awaits the push.
pub async fn seen(State(app): State<Shared>, Path(id): Path<String>) -> Result<StatusCode, crate::AppError> {
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let mut cleared = false;
    let _ = app
        .update_session(&id, |s| {
            cleared = s.unseen_failure;
            s.unseen_failure = false;
        })
        .await;
    if cleared {
        spawn_resolved(&app, &id);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Fires [`crate::push::resolved`] for one colony, spawned: an answer or a look must never wait
/// on the push services, and a failed push costs nothing but a log line on the device's side.
fn spawn_resolved(app: &Shared, id: &str) {
    let app = app.clone();
    let id = id.to_string();
    tokio::spawn(async move { crate::push::resolved(&app, &id).await });
}

/// A scoped API token's free text, prefixed with the external-input marker (issue #508): the agent
/// reads it as a description of the task from outside, never as the operator's voice. Shared by
/// the answer paths' `response` note and the socket's `user_message`. An empty note leaves the
/// marker alone, so the agent still sees who acted.
fn external_text(name: &str, text: &str) -> String {
    if text.trim().is_empty() {
        format!("[external input from API token \"{name}\"]")
    } else {
        format!("[external input from API token \"{name}\"] {text}")
    }
}

/// `POST /api/sessions/{id}/messages` (issue #746): the HTTP twin of the events socket's
/// `user_message` command, for the phone's offline queue — a POST whose response was lost is
/// repeated with the same client `id`, and the repeat is answered, not delivered twice. Same
/// shared path as the socket ([`submit_message`]), the answer twin's error shapes.
pub async fn message(
    State(app): State<Shared>,
    Path(id): Path<String>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, crate::AppError> {
    let client = body["id"].as_str().unwrap_or_default();
    if !valid_client_id(client) {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "expected {\"id\": str of 1-64 of A-Z a-z 0-9 _ -, \"text\": str}",
        ));
    }
    let rt = app.runtime(&id).await;
    let scoped = scoped.map(|axum::Extension(scoped)| scoped);
    match submit_message(
        &app,
        &id,
        &rt,
        body["text"].as_str().unwrap_or_default(),
        Some(client),
        scoped.as_ref(),
    )
    .await
    {
        Ok(sent) => Ok(Json(json!({"id": sent.id, "duplicate": !sent.delivered}))),
        Err(MessageError::NoSession) => Err(client_error(StatusCode::NOT_FOUND, "no such session")),
        Err(MessageError::NotAccepting(status)) => Err(client_error(
            StatusCode::CONFLICT,
            &format!("the colony is {} and cannot take a message; resume it first", status.as_str()),
        )),
        Err(MessageError::Undelivered) => Err(client_error(
            StatusCode::CONFLICT,
            "the colony is stopping and cannot take a message; retry once it has resumed",
        )),
        Err(MessageError::Invalid) => Err(client_error(
            StatusCode::BAD_REQUEST,
            "expected a non-empty \"text\" of at most 100,000 characters",
        )),
    }
}

/// The client id the messages twin dedupes on: 1-64 of ASCII letters, digits, `_` and `-`, so it
/// is safe to embed in the wire id (`u-<client>`) and never reads as anything but an id.
fn valid_client_id(client: &str) -> bool {
    (1..=64).contains(&client.len()) && client.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Client ids already delivered, `(colony, client id)` oldest first. Bounded at
/// [`SEEN_CLIENTS_MAX`] pairs: a repeat inside the window is answered, not delivered twice; past
/// the window it delivers again, which a queue's retry (seconds, not days) never reaches.
static SEEN_CLIENTS: LazyLock<std::sync::Mutex<VecDeque<(String, String)>>> =
    LazyLock::new(|| std::sync::Mutex::new(VecDeque::new()));
const SEEN_CLIENTS_MAX: usize = 512;

/// Why a user message was not delivered. The socket path drops each of these silently, as it
/// always has; the HTTP twin answers each with its own status.
#[derive(Debug)]
pub(crate) enum MessageError {
    NoSession,
    NotAccepting(SessionStatus),
    /// Empty or oversized text — the socket drops it, HTTP answers 400.
    Invalid,
    /// The send into the runner's channel failed (its receiver is gone — the colony is stopping):
    /// nothing was delivered, and nothing was remembered. The socket drops it as ever; HTTP
    /// answers a 409 so the queue's retry is not absorbed as a duplicate.
    Undelivered,
}

/// What one user message turned into, for the HTTP twin's reply.
pub(crate) struct Sent {
    pub(crate) id: String,
    pub(crate) delivered: bool,
}

/// The one user-message path (issue #746): the events socket's `user_message` command and
/// `POST /api/sessions/{id}/messages` both forward through here, so both check the colony, trim
/// and size-check the text, and mark a scoped token's message the same way. `client_id` is the
/// HTTP twin's dedupe key; the socket passes `None` and mints its own `u-<short_id>`.
pub(crate) async fn submit_message(
    app: &Shared,
    id: &str,
    rt: &Arc<Runtime>,
    raw_text: &str,
    client_id: Option<&str>,
    scoped: Option<&crate::api_tokens::ScopedToken>,
) -> Result<Sent, MessageError> {
    let s = app.session(id).await.ok_or(MessageError::NoSession)?;
    if !accepts_commands(s.status) {
        return Err(MessageError::NotAccepting(s.status));
    }
    let text = raw_text.trim();
    if text.is_empty() || text.len() > 100_000 {
        return Err(MessageError::Invalid);
    }
    // A scoped token's message is external input like its answers (issue #508): the marker tells
    // the agent who is talking, in the operator's voice or not.
    let text = match scoped {
        Some(token) => external_text(&token.name, text),
        None => text.to_string(),
    };
    let mid = match client_id {
        Some(client) => format!("u-{client}"),
        None => format!("u-{}", short_id()),
    };
    let forward = json!({"type": "user_message", "id": mid, "text": text});
    let Some(client) = client_id else {
        rt.commands.send(forward).map_err(|_| MessageError::Undelivered)?;
        return Ok(Sent {
            id: mid,
            delivered: true,
        });
    };
    // A repeat client id is answered, not delivered twice. The check, the send and the record are
    // one step under the lock (the send is a synchronous channel push), so two retries racing each
    // other cannot both deliver. A fresh id is recorded only once its send has gone through: the
    // channel dies with the runner (a stopping colony), and an id remembered ahead of a failed
    // send would read every retry as a duplicate — the message lost without a trace.
    let mut seen = SEEN_CLIENTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if seen.iter().any(|(s, c)| s == id && c == client) {
        return Ok(Sent {
            id: mid,
            delivered: false,
        });
    }
    rt.commands.send(forward).map_err(|_| MessageError::Undelivered)?;
    seen.push_back((id.to_string(), client.to_string()));
    while seen.len() > SEEN_CLIENTS_MAX {
        seen.pop_front();
    }
    Ok(Sent {
        id: mid,
        delivered: true,
    })
}

/// An answer as the API takes it: the question it answers, one choice per question label, and the
/// free-text note the agent reads alongside. Exactly the socket command's fields (`client_command`),
/// parsed once so both paths validate identically. The push-answer route (issue #742) builds one
/// directly, so the shape is crate-visible. `questions`, optional, is the question content the
/// answerer saw (issue #746): a queued answer replayed after a reconnect carries it, so an answer
/// to a question that has since changed under the same id is refused, never delivered.
pub(crate) struct AnswerCommand {
    pub(crate) question_id: String,
    pub(crate) answers: Value,
    pub(crate) response: Value,
    pub(crate) questions: Option<Vec<Value>>,
}

impl AnswerCommand {
    /// `None` when the body is not shaped like an answer at all — a missing id, or answers that are
    /// not an object of option labels. Whether the labels actually match the open question is the
    /// runner's to find out, here as over the wire.
    fn parse(command: &Value) -> Option<AnswerCommand> {
        Some(AnswerCommand {
            question_id: command["question_id"].as_str()?.to_string(),
            answers: command["answers"].as_object()?.clone().into(),
            response: command.get("response").cloned().unwrap_or(Value::Null),
            questions: command.get("questions").and_then(Value::as_array).cloned(),
        })
    }

    /// The command as the runner receives it. `external` — the name of a scoped API token —
    /// prefixes the free-text note so the agent knows the answer came from outside the cockpit
    /// (issue #508): instructions from an external token are a description of the task, not the
    /// operator's voice. The marker goes into `response` only, never into `answers`, whose labels
    /// must match the question's options exactly.
    fn forward(self, external: Option<&str>) -> Value {
        let response = match external {
            Some(name) => Value::String(external_text(name, self.response.as_str().unwrap_or_default())),
            None => self.response,
        };
        json!({
            "type": "answer",
            "question_id": self.question_id,
            "answers": self.answers,
            "response": response,
        })
    }
}

/// Why an HTTP answer was refused. Each refusal names itself in the handler above: a colony that
/// is not there is a 404; one that cannot take an answer, is not asking, or is asking something
/// else is a 409 — a conflict with the colony's state that re-reading the question resolves.
pub(crate) enum AnswerError {
    NoSession,
    NotAccepting(SessionStatus),
    NoQuestion,
    Stale,
}

/// The one answer path (issue #508): the events socket's `answer` command and
/// `POST /api/sessions/{id}/answer` both forward through here, so both check the colony the same
/// way and write the same activity line. `require_pending` is the HTTP endpoint's extra guard —
/// over the wire the runner owns the question's lifecycle and refuses an answer whose question has
/// closed, so the socket path does not pre-check; over HTTP a stale id is a 409, because an HTTP
/// caller cannot otherwise tell "delivered" from "answered nothing".
///
/// A suspended colony takes answers too (issue #562) — it is exactly the colony whose question
/// must stay answerable — but has no runner to hand one to, so the answer is held on the record
/// ([`hold_answer`]) until a boot delivers it. A colony not yet suspended forwards to its live
/// runner under the open question's lock — the same lock the suspension tick claims under — so an
/// answer and a suspension cannot interleave: either the answer goes first and the tick leaves the
/// colony alone, or the claim goes first and the answer is held.
pub(crate) async fn submit_answer(
    app: &Shared,
    id: &str,
    rt: &Arc<Runtime>,
    answer: AnswerCommand,
    via: Option<crate::auth::Via>,
    external: Option<&str>,
    require_pending: bool,
) -> Result<(), AnswerError> {
    let s = app.session(id).await.ok_or(AnswerError::NoSession)?;
    let suspended = s.suspended.is_some();
    // A colony parked by the hold timeout still holds the question it parked on (issue #876): an
    // answer resumes it with the answer. Checked before the not-accepting refusal below, which a
    // parked colony would otherwise hit.
    if !accepts_commands(s.status)
        && !suspended
        && crate::queue::hold_parked(&s)
        && s.parked.as_ref().is_some_and(|p| p.question_risk.is_some())
    {
        return answer_parked(app, id, rt, answer, external, via).await;
    }
    if !accepts_commands(s.status) && !suspended {
        return Err(AnswerError::NotAccepting(s.status));
    }
    let open = rt.open_question().await;
    // Over the socket the runner owns the question's lifecycle and refuses a stale answer itself;
    // a suspended colony has no runner to do that, so the host checks here whatever path brought
    // the answer in.
    if require_pending || suspended {
        // Ids alone do not name one question for life — runners count `q-1`, `q-2`… afresh when a
        // suspended colony boots again — so an answer that says what it saw must match that too.
        let open_matches = open.as_ref().is_some_and(|(open_id, questions, _)| {
            open_id == &answer.question_id && answer.questions.as_ref().is_none_or(|saw| saw == questions)
        });
        if !open_matches {
            return Err(if open.is_some() {
                AnswerError::Stale
            } else {
                AnswerError::NoQuestion
            });
        }
    }
    if suspended {
        return hold_answer(app, id, rt, open, answer, external, via).await;
    }
    // The suspension tick's gate (issue #562): the tick claims a colony for suspension holding the
    // open question's lock, so this re-check, the taking-down of the question and the send are one
    // step beside it. If the tick claimed first, the colony reads suspended here and the answer is
    // held for the restore instead of being sent into the link the tick is tearing down; if this
    // path goes first, the question reads taken-down in the tick and the colony is left alone, its
    // answer in flight to a runner that keeps its microVM.
    let mut gate = rt.open_question.lock().await;
    let Some(s) = app.session(id).await else {
        return Err(AnswerError::NoSession);
    };
    if s.suspended.is_some() {
        drop(gate);
        return hold_answer(app, id, rt, open, answer, external, via).await;
    }
    // An HTTP answer (require_pending) that lost the race for the question — a push or cockpit tap
    // that took it down between the open read above and this lock — stops here: forwarding it too
    // would hand the runner a second answer to a question the host no longer counts as open
    // (issue #742's promise: a notification tap and the cockpit's button answer a colony once).
    // The socket path keeps forwarding whatever it is given: the host may not know the question it
    // answers yet, and the runner refuses what is stale.
    let gate_matches = gate.as_ref().is_some_and(|(open_id, ..)| open_id == &answer.question_id);
    if require_pending && !gate_matches {
        return Err(if open.is_some() {
            AnswerError::Stale
        } else {
            AnswerError::NoQuestion
        });
    }
    // The answer is on its way to the runner, whose own `question_answered` echo closes the
    // question: close it here first, so the tick — which claims only under an open question, and
    // matching the one the answer is for — reads none. A stale answer (socket path) matches
    // nothing and leaves the live question standing for the runner to refuse it.
    if gate_matches {
        *gate = None;
    }
    drop(gate);
    // A send into a dead runner (the colony stopping between the checks above and here) delivers
    // nothing: the HTTP twin answers the not-accepting 409 instead of a 204 for a lost answer,
    // while the socket path drops the error as it always has.
    // The question bodies the decision is read from (issue #1247): what the host has open, or —
    // the socket path, where the host may not have opened the question — what the answerer saw.
    let decided = (
        answer.question_id.clone(),
        open.as_ref()
            .map(|(_, questions, _)| questions.clone())
            .or_else(|| answer.questions.clone())
            .unwrap_or_default(),
        answer.answers.clone(),
        match external {
            Some(name) => external_text(name, answer.response.as_str().unwrap_or_default()),
            None => answer.response.as_str().unwrap_or_default().to_string(),
        },
    );
    if rt.commands.send(answer.forward(external)).is_err() {
        return Err(AnswerError::NotAccepting(s.status));
    }
    let (question_id, questions, answers, response) = decided;
    record_decision(app, id, &question_id, &questions, &answers, Some(&response)).await;
    crate::activity::record_answer(app, &s, via).await;
    // The question is answered as far as the person is concerned (issue #744): every other
    // device closes its notification and drops the colony from its badge.
    spawn_resolved(app, id);
    Ok(())
}

/// Puts an operator's decision on the colony's record (issue #1247): the question, the choice and
/// the free-text note, so a restart's resume hands them back to the agent and a re-asked question
/// is answered from the record instead of the person. Every operator answer path records here —
/// the judge's answers in `autonomy.rs` never do. Best effort: a colony that vanished between the
/// answer and this write loses only the reuse of the answer, never the answer itself.
async fn record_decision(
    app: &Shared,
    id: &str,
    question_id: &str,
    questions: &[Value],
    answers: &Value,
    response: Option<&str>,
) {
    app.update_session(id, |x| x.record_answered_question(question_id, questions, answers, response))
        .await;
}

/// The user message a resumed runner receives for a suspended colony's answer (issue #562): the
/// question as it was asked, then the choices made, then the free-text note. Pure, so tests pin the
/// wording the agent reads.
fn answer_prompt(questions: &[Value], answers: &Value, response: &str) -> String {
    answer_body(
        "Earlier you asked the user something, and this colony was suspended while it waited (its \
         microVM was stopped to free its slot). The conversation continues now — this is their answer.",
        questions,
        answers,
        response,
    )
}

/// The note a colony parked by the hold timeout resumes on when its answer arrives (issue #876): the
/// same replay, opened with what happened instead of a suspension's wording.
fn parked_answer_prompt(questions: &[Value], answers: &Value, response: &str) -> String {
    answer_body(
        "While you were parked, your question was answered. This is the person's answer.",
        questions,
        answers,
        response,
    )
}

/// The shared body of the two answer prompts above: the opening line, one `Q:`/`A:` pair per question,
/// then the free-text note when there is one.
fn answer_body(intro: &str, questions: &[Value], answers: &Value, response: &str) -> String {
    let mut lines = vec![intro.to_string()];
    let given = |text: &str| -> Option<String> {
        let map = answers.as_object()?;
        match map.get(text) {
            Some(Value::String(label)) => Some(label.clone()),
            Some(Value::Array(labels)) => Some(labels.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")),
            _ => None,
        }
    };
    for q in questions {
        let Some(text) = q["question"].as_str().filter(|t| !t.trim().is_empty()) else {
            continue;
        };
        match given(text) {
            Some(choice) => lines.push(format!("Q: {text}\nA: {choice}")),
            None => lines.push(format!("Q: {text}\nA: (no choice given)")),
        }
    }
    let note = response.trim();
    if !note.is_empty() {
        lines.push(format!("Their note: {note}"));
    }
    lines.join("\n")
}

/// The suspended colony's answer path (issue #562): hold the answer on the record — persisted by
/// the write before this returns — close the question in the event log so a restart's replay does
/// not re-open it, and leave the delivery to the queue's restore (or a manual Resume). `Err` back
/// means the answer was not held: the colony was claimed out of its suspension in between (a
/// restore's boot is already carrying an answer) or has vanished — telling the caller is what keeps
/// the answer from being silently dropped.
async fn hold_answer(
    app: &Shared,
    id: &str,
    rt: &Arc<Runtime>,
    open: Option<(String, Vec<Value>, QuestionRisk)>,
    answer: AnswerCommand,
    external: Option<&str>,
    via: Option<crate::auth::Via>,
) -> Result<(), AnswerError> {
    // The free-text note carries the external-input marker exactly as the live path's `forward` does.
    let response = match external {
        Some(name) => external_text(name, answer.response.as_str().unwrap_or_default()),
        None => answer.response.as_str().unwrap_or_default().to_string(),
    };
    let questions = open.map(|(_, questions, _)| questions).unwrap_or_default();
    let prompt = answer_prompt(&questions, &answer.answers, &response);
    // Conditional on purpose: a restore or a stop that claimed the colony between the caller's
    // snapshot and this write must not hang a pending answer on a colony that is no longer
    // suspended. `false` back means exactly that happened.
    let stored = app
        .update_session(id, |x| {
            let was = x.suspended.is_some();
            if was {
                x.pending_answer = Some(PendingAnswer {
                    question_id: answer.question_id.clone(),
                    prompt: prompt.clone(),
                    answered_at: Some(Utc::now()),
                });
            }
            was
        })
        .await;
    match stored {
        Some((x, true)) => {
            // The question is closed as of now, in the same terms the runner closes it, so a
            // restart's replay (which skips host chain lines only for the reconnect cursor, not
            // for the question) finds no question still open. The answers travel too, so the
            // transcript keeps what was chosen.
            crate::validation::emit_chain(
                app,
                id,
                json!({
                    "type": "question_answered",
                    "question_id": answer.question_id,
                    "answers": answer.answers,
                    "response": response,
                }),
            )
            .await;
            *rt.open_question.lock().await = None;
            rt.question_holds_tool_call.store(false, std::sync::atomic::Ordering::SeqCst);
            rt.activity.lock().await.question_since = None;
            record_decision(app, id, &answer.question_id, &questions, &answer.answers, Some(&response)).await;
            if let Some(s) = app.session(id).await {
                crate::activity::record_answer(app, &s, via).await;
            }
            // The held answer settles the question the same way a live one does (issue #744).
            spawn_resolved(app, id);
            // Where the colony stands in the restore line (issue #667), read off the same admission
            // the restore pass answers to, so the log says what the next ticks will do with it.
            // The pause is the tick's own hold on the restore pass (`start_queued` skips it while
            // either pause stands), so the note never promises a resume the tick cannot make.
            let modules = app.modules.read().await.clone();
            let paused = crate::reclaim::admission_paused(app).await || crate::providers::quota_status(app).await.paused;
            // Resolved before the admission read: `org_settings` reads the orgs file with blocking IO.
            let org_settings = app.org_settings(&x.org);
            // Resolved before the sessions lock: auto mode reads the sessions itself.
            let max_parallel = crate::capacity::max_parallel(app, &modules).await;
            let note = {
                let sessions = app.sessions.read().await;
                crate::queue::restore_line_note(
                    &sessions,
                    &x,
                    max_parallel,
                    orgs::org_max_parallel(&org_settings),
                    crate::queue::repo_limit(&modules, &org_settings),
                    paused,
                )
            };
            app.session_log(
                id,
                "info",
                format!("answer received while suspended; it is kept on the colony and delivered when it resumes; {note}"),
            )
            .await;
        }
        // A restore claimed the colony in between: the boot in flight carries the earlier answer,
        // and `rt` is the very runtime that restore retired — a message sent into it would vanish
        // with the link it was tearing down. Refuse instead (a 409 over HTTP), so the answer is
        // not silently dropped; answered again once the fresh runner is up, it goes straight down
        // the new link.
        Some((x, false)) => return Err(AnswerError::NotAccepting(x.status)),
        // The colony is gone: the same answer an unknown id gets.
        None => return Err(AnswerError::NoSession),
    }
    Ok(())
}

/// The answer path for a colony parked by the hold timeout (issue #876): the question it parked on is
/// still the one on the record, so the answer is kept as the resume note and the colony resumes at
/// once, queued when no slot is free. The question is closed in the event log, as a suspended
/// colony's answer is, so a restart's replay finds none still open.
async fn answer_parked(
    app: &Shared,
    id: &str,
    rt: &Arc<Runtime>,
    answer: AnswerCommand,
    external: Option<&str>,
    via: Option<crate::auth::Via>,
) -> Result<(), AnswerError> {
    let open = rt.open_question().await;
    // An answer that says what it saw must match the question still on the runtime, exactly as the
    // suspended path checks — ids alone do not name one question.
    let open_matches = open.as_ref().is_some_and(|(open_id, questions, _)| {
        open_id == &answer.question_id && answer.questions.as_ref().is_none_or(|saw| saw == questions)
    });
    if !open_matches {
        return Err(if open.is_some() {
            AnswerError::Stale
        } else {
            AnswerError::NoQuestion
        });
    }
    let (_, questions, _) = open.expect("open_matches checked the question");
    // The free-text note carries the external-input marker exactly as the live path's `forward` does.
    let response = match external {
        Some(name) => external_text(name, answer.response.as_str().unwrap_or_default()),
        None => answer.response.as_str().unwrap_or_default().to_string(),
    };
    let note = parked_answer_prompt(&questions, &answer.answers, &response);
    // Conditional on purpose: a backoff resume or an operator's press that claimed the colony
    // between the caller's snapshot and this write must not hang a note on a colony that is no
    // longer parked — `false` says exactly that happened.
    let stored = app
        .update_session(id, |x| {
            if !crate::queue::hold_parked(x) {
                return false;
            }
            x.resume_note = Some(note.clone());
            true
        })
        .await;
    match stored {
        Some((_x, true)) => {
            // The question is closed as of now, in the same terms the runner closes it, so a
            // restart's replay finds no question still open. The answers travel too, so the
            // transcript keeps what was chosen.
            crate::validation::emit_chain(
                app,
                id,
                json!({
                    "type": "question_answered",
                    "question_id": answer.question_id,
                    "answers": answer.answers,
                    "response": response,
                }),
            )
            .await;
            *rt.open_question.lock().await = None;
            rt.question_holds_tool_call.store(false, std::sync::atomic::Ordering::SeqCst);
            rt.activity.lock().await.question_since = None;
            record_decision(app, id, &answer.question_id, &questions, &answer.answers, Some(&response)).await;
            if let Some(s) = app.session(id).await {
                crate::activity::record_answer(app, &s, via.clone()).await;
            }
            // The answer settles the question the same way a live one does (issue #744).
            spawn_resolved(app, id);
            app.session_log(id, "info", "answer received while parked; resuming the colony with it".into())
                .await;
            // 409/404 back means the park is no longer this answer's to resume — a resume won the
            // race — and the answer stays on the record for that resume's boot to deliver.
            let _ = crate::lifecycle::resume(State(app.clone()), Path(id.to_string()), via.map(axum::Extension)).await;
            Ok(())
        }
        Some((x, false)) => Err(AnswerError::NotAccepting(x.status)),
        None => Err(AnswerError::NoSession),
    }
}

#[derive(Deserialize)]
pub struct SinceQuery {
    since: Option<u64>,
    epoch: Option<u64>,
    /// Issue #1210: how many events a first paint (the socket) or a page (plain GET) carries. A
    /// socket without it replays the whole run, as before.
    limit: Option<usize>,
    /// A page of the events before this `seq` (of run `epoch`, at byte `offset` when known).
    before: Option<u64>,
    offset: Option<u64>,
}

/// Who is on a colony's events socket, and what their token allows (issue #508): the `Via` the
/// activity line names and the scoped token whose scope gates driving commands. Both `None` for
/// the owner — the browser or a request holding the install token.
#[derive(Clone, Default)]
struct SocketActor {
    via: Option<crate::auth::Via>,
    scoped: Option<crate::api_tokens::ScopedToken>,
}

pub async fn events_ws(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<SinceQuery>,
    via: Option<axum::Extension<crate::auth::Via>>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
    revocation: Option<axum::Extension<crate::auth::Revocation>>,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Result<Response, crate::AppError> {
    let revocation = revocation.map(|axum::Extension(r)| r);
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    // A plain GET is a page of the log (issue #1210); only an upgrade opens the live socket.
    let Ok(ws) = ws else {
        return events_page(&app, &id, &query).await;
    };
    let rt = app.runtime(&id).await;
    let via = via.map(|axum::Extension(via)| via);
    let scoped = scoped.map(|axum::Extension(scoped)| scoped);
    let actor = SocketActor { via, scoped };
    // A revoked credential's socket closes at once (issue #746), not at its next request.
    Ok(ws.on_upgrade(move |socket| {
        crate::auth::Revocation::until(
            revocation,
            events_socket(app, id, rt, query.since.unwrap_or(0), query.epoch, query.limit, actor, socket),
        )
    }))
}

/// One replayable line of events.jsonl: the decoded line and its seq, or `None` to skip it.
/// Decoded one chunk at a time, as `Runtime::load` reads the same file: a non-UTF-8 line or a
/// line outside the JSON contract costs itself, not the rest of the transcript.
pub(crate) fn replay_line(chunk: &[u8]) -> Option<(u64, &str)> {
    let line = std::str::from_utf8(chunk).ok()?;
    let seq = serde_json::from_str::<Value>(line).ok().and_then(|v| v["seq"].as_u64())?;
    Some((seq, line))
}

/// `GET /api/sessions/{id}/events` without an upgrade (issue #1210): the newest `limit` events, or
/// with `before=<seq>` (and the `epoch` and `offset` the previous page named) the page before them.
/// Read from the end of the log, stepping back through the rotated `events-N.jsonl`, so the cost of
/// a page does not grow with the history behind it.
async fn events_page(app: &Shared, id: &str, query: &SinceQuery) -> Result<Response, crate::AppError> {
    let current = run_epoch(app.store(), id).await;
    let before = query.before.map(|seq| history::Before {
        epoch: query.epoch.filter(|e| *e != 0).unwrap_or(current),
        seq,
        offset: query.offset,
    });
    let limit = query.limit.unwrap_or(history::DEFAULT_LIMIT);
    let page = history::page(app.store(), id, current, before, limit, true)
        .await
        .map_err(|e| {
            client_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not read the event log: {e}"),
            )
        })?;
    let mut body = page.meta();
    body["events"] = Value::Array(page.events);
    body["run_epoch"] = json!(current);
    Ok(Json(body).into_response())
}

#[allow(clippy::too_many_arguments)]
async fn events_socket(
    app: Shared,
    id: String,
    rt: Arc<Runtime>,
    since: u64,
    client_epoch: Option<u64>,
    limit: Option<usize>,
    actor: SocketActor,
    socket: WebSocket,
) {
    // Before this socket subscribes and the log ring is drained, so its alert lands in the
    // drained history once instead of arriving twice.
    app.report_load_error(&id, &rt).await;
    let (mut tx, mut rx) = socket.split();
    let mut subscription = rt.events.subscribe();
    let mut retired = rt.retired.subscribe();
    let text = |s: String| Message::Text(s.into());

    // First frame on the wire, before the session frame and any replay: the run epoch this
    // connection is attached to, so a tab left open across a resume learns its cursor belongs to
    // a retired run. It carries no `seq` field, so it passes seq filtering like `harness_log`,
    // and old clients ignore the unknown frame.
    let current_epoch = run_epoch(app.store(), &id).await;
    if tx
        .send(text(json!({"type": "run_epoch", "epoch": current_epoch}).to_string()))
        .await
        .is_err()
    {
        return;
    }
    let Some(session) = app.session(&id).await else { return };
    let session = with_activity(&app, session).await;
    if tx
        .send(text(json!({"type": "session", "session": session}).to_string()))
        .await
        .is_err()
    {
        return;
    }
    let logs: Vec<Value> = rt.logs.lock().await.iter().cloned().collect();
    for entry in logs {
        if tx.send(text(entry.to_string())).await.is_err() {
            return;
        }
    }
    // A `since` from a retired run is a rank in that run's per-run numbering, meaningless in the
    // new run: replay from the epoch-adjusted cursor instead, and seed the live dedupe cursor
    // with it so the new run's first events are neither dropped nor duplicated.
    let effective = effective_since(client_epoch, current_epoch, since);
    let mut replayed = effective;
    // Byte-split, never `BufReader::lines()`: `next_line()` reports a non-UTF-8 line as an
    // `InvalidData` error, which reads exactly like EOF here and would silently truncate the
    // replay at the first corrupt line. Split chunks decode (or skip) one at a time instead.
    let mut skipped = 0u64;
    // A first paint with a `limit` (issue #1210) is the newest page of this run, a `history` frame
    // saying whether more is behind it, and the summary of what the older part held. A reconnect
    // (`since` > 0) only needs what it missed, which is small, and replays it as before.
    let mut tailed = false;
    if let Some(limit) = limit.filter(|_| effective == 0) {
        match history::page(app.store(), &id, current_epoch, None, limit, false).await {
            Ok(page) => {
                let summary = rt.summary(app.store(), &id).await;
                let mut frame = page.meta();
                frame["type"] = json!("history");
                frame["summary"] = json!(summary);
                if tx.send(text(frame.to_string())).await.is_err() {
                    return;
                }
                for event in &page.events {
                    replayed = replayed.max(event["seq"].as_u64().unwrap_or(0));
                    if tx.send(text(event.to_string())).await.is_err() {
                        return;
                    }
                }
                tailed = true;
            }
            Err(e) => {
                app.session_log(
                    &id,
                    "warn",
                    format!("could not read the newest events ({e}); replaying the whole log"),
                )
                .await;
            }
        }
    }
    if !tailed && let Ok(Some(bytes)) = app.store().read_file(&id, "events.jsonl").await {
        for chunk in bytes.split(|b| *b == b'\n') {
            if chunk.is_empty() {
                continue;
            }
            let Some((seq, line)) = replay_line(chunk) else {
                skipped += 1;
                continue;
            };
            if seq > effective {
                if tx.send(text(line.to_string())).await.is_err() {
                    return;
                }
                replayed = replayed.max(seq);
            }
        }
    }
    if skipped > 0 {
        // A gap in the event log must not be silent: the entry lands in the harness log and on
        // the socket (via session_log's broadcast, picked up by the live loop below) so the
        // transcript visibly continues past the gap.
        app.session_log(
            &id,
            "warn",
            format!(
                "skipped {} unreadable {} in events.jsonl during replay; the transcript continues past the gap",
                skipped,
                if skipped == 1 { "line" } else { "lines" },
            ),
        )
        .await;
    }
    // The backlog is on the wire: the browser holds its render until this frame, so a long
    // history opens on its latest messages instead of filling in line by line. No `seq`, like
    // `run_epoch`, and old clients ignore the unknown frame.
    if tx
        .send(text(json!({"type": "replay_done", "seq": replayed}).to_string()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            item = subscription.recv() => match item {
                Ok(item) => {
                    if item.seq.is_some_and(|seq| seq <= replayed) {
                        continue;
                    }
                    if tx.send(text(item.json.clone())).await.is_err() {
                        return;
                    }
                }
                // Too slow to keep up: close so the client reconnects with `since`.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = tx.send(Message::Close(None)).await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = retired.changed() => {
                // The colony resumed: this socket holds the retired run's Runtime and can never
                // see the new run's events, so close and let the client reconnect for the new epoch.
                let _ = tx.send(Message::Close(None)).await;
                return;
            },
            message = rx.next() => match message {
                Some(Ok(Message::Text(body))) => {
                    client_command(&app, &id, &rt, actor.via.clone(), actor.scoped.clone(), body.as_str()).await
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

/// Whether a colony can still be sent a message, an answer or an interrupt.
///
/// The microVM has to be up: once a colony is publishing, stopped or finished
/// there is no agent to receive it.
fn accepts_commands(status: SessionStatus) -> bool {
    status.is_live()
}

async fn client_command(
    app: &Shared,
    id: &str,
    rt: &Arc<Runtime>,
    via: Option<crate::auth::Via>,
    scoped: Option<crate::api_tokens::ScopedToken>,
    body: &str,
) {
    let Ok(command) = serde_json::from_str::<Value>(body) else {
        return;
    };
    // Every command this socket takes drives the colony: a `read`-scoped token may watch the
    // transcript but not act on it (issue #508). Refused with a warn on the transcript rather than
    // dropped, so a misconfigured client sees why nothing happens.
    if let Some(token) = &scoped
        && token.scope < crate::api_tokens::Scope::Operate
    {
        app.session_log(
            id,
            "warn",
            format!(
                "this API token's scope ({}) can watch this colony but not drive it; answering, interrupting or messaging needs an operate or launch token",
                token.scope.as_str()
            ),
        )
        .await;
        return;
    }
    let Some(s) = app.session(id).await else { return };
    if !accepts_commands(s.status) {
        // Dropping it silently left the browser showing an answer on its way to an
        // agent that is gone. Send the session back instead: a client whose view is
        // stale corrects itself, and its card stops offering to answer.
        let view = with_activity(app, s.clone()).await;
        rt.broadcast(None, json!({"type": "session", "session": view}).to_string());
        return;
    }
    let forward = match command["type"].as_str() {
        Some("user_message") => {
            // The shared user-message path (issue #746): no client id over the socket, so every
            // message delivers. A refusal here is the stale-session broadcast above, as before.
            let _ = submit_message(
                app,
                id,
                rt,
                command["text"].as_str().unwrap_or_default(),
                None,
                scoped.as_ref(),
            )
            .await;
            return;
        }
        Some("answer") => {
            // The socket path pre-checks nothing about the question: the runner owns the
            // lifecycle and refuses a stale answer itself (see `submit_answer`).
            let Some(parsed) = AnswerCommand::parse(&command) else {
                return;
            };
            let external_name = match &via {
                Some(crate::auth::Via::Token(name)) => Some(name.clone()),
                _ => None,
            };
            let external = external_name.as_deref();
            let _ = submit_answer(app, id, rt, parsed, via, external, false).await;
            return;
        }
        Some("interrupt") => {
            rt.interrupted.store(true, Ordering::SeqCst);
            // The UHP response this interrupt ends reads `cancelled`, not failed (§7.4).
            crate::uhp_responses::note_interrupt(app, id).await;
            json!({"type": "interrupt"})
        }
        Some("set_model") => {
            // Switching the model is not in the issue's operate list (issue #508): a token may
            // drive the colony's work but not change what it runs on. Warned on the transcript,
            // not dropped, so the client sees why nothing happened.
            if scoped.is_some() {
                app.session_log(
                    id,
                    "warn",
                    "switching a colony's model stays with the maintainer; an API token, whatever its scope, cannot change it"
                        .to_string(),
                )
                .await;
                return;
            }
            let Some(model) = set_model_id(command["model"].as_str().unwrap_or_default()) else {
                return;
            };
            json!({"type": "set_model", "model": model})
        }
        _ => return,
    };
    let _ = rt.commands.send(forward);
}

/// The model a `set_model` switches the colony to, trimmed, or `None` to drop the command.
///
/// Only the shape is checked, in the forms a model setting takes (§6.1): a Claude alias or ID,
/// a `[1m]` suffix, `<provider>/<model>`, same characters as a provider's model list. Whether the
/// id resolves is the colony's to find out against the routes it booted with, and a refused switch
/// comes back as a `warn` log.
fn set_model_id(raw: &str) -> Option<&str> {
    // The longest id `/api/models` lists, so every model the picker offers gets through:
    // `<provider id>/<model>`, a provider id of at most 32 bytes (providers.rs `valid_id`) and a
    // model of at most 120 (`valid_model`).
    const MAX_MODEL_ID: usize = 32 + 1 + 120;
    let model = raw.trim();
    let shaped =
        (1..=MAX_MODEL_ID).contains(&model.len()) && model.chars().all(|c| c.is_ascii_alphanumeric() || "._:-/[]".contains(c));
    shaped.then_some(model)
}

#[derive(Deserialize)]
pub struct TerminalQuery {
    cols: Option<u16>,
    rows: Option<u16>,
}

pub async fn terminal_ws(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<TerminalQuery>,
    revocation: Option<axum::Extension<crate::auth::Revocation>>,
    ws: WebSocketUpgrade,
) -> Result<Response, crate::AppError> {
    let revocation = revocation.map(|axum::Extension(r)| r);
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let cols = query.cols.unwrap_or(80).clamp(10, 500);
    let rows = query.rows.unwrap_or(24).clamp(5, 300);
    Ok(ws.on_upgrade(move |socket| crate::auth::Revocation::until(revocation, terminal_socket(app, s, cols, rows, socket))))
}

async fn terminal_socket(app: Shared, s: Session, cols: u16, rows: u16, mut socket: WebSocket) {
    let not_ready = match s.status {
        SessionStatus::Starting => Some("the colony is still starting; the terminal opens once its microVM is ready"),
        status if !status.is_live() => Some("the colony's microVM isn't running"),
        _ => None,
    };
    if let Some(message) = not_ready {
        let _ = socket
            .send(Message::Text(json!({"type": "error", "message": message}).to_string().into()))
            .await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    let upstream = match agentd_ws(&app, &s, &format!("/v1/pty?cols={cols}&rows={rows}")).await {
        Ok(ws) => ws,
        Err(e) => {
            let message = json!({"type": "error", "message": format!("can't open a terminal in the microVM: {e:#}")});
            let _ = socket.send(Message::Text(message.to_string().into())).await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let (mut up_tx, mut up_rx) = upstream.split();
    let (mut down_tx, mut down_rx) = socket.split();
    loop {
        tokio::select! {
            message = down_rx.next() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    if up_tx.send(tungstenite::Message::Binary(bytes.to_vec().into())).await.is_err() { break }
                }
                Some(Ok(Message::Text(body))) => {
                    if up_tx.send(tungstenite::Message::Text(body.as_str().into())).await.is_err() { break }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            message = up_rx.next() => match message {
                Some(Ok(tungstenite::Message::Binary(bytes))) => {
                    if down_tx.send(Message::Binary(bytes.to_vec().into())).await.is_err() { break }
                }
                Some(Ok(tungstenite::Message::Text(body))) => {
                    if down_tx.send(Message::Text(body.as_str().into())).await.is_err() { break }
                }
                Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = up_tx.close().await;
    let _ = down_tx.close().await;
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    super::files::routes()
        .route("/api/sessions", routing::get(list).post(create))
        .route("/api/sessions/{id}", routing::get(get))
        .route("/api/sessions/{id}/commits", routing::get(crate::commit_links::api_commits))
        .route("/api/sessions/{id}/question", routing::get(question))
        .route("/api/sessions/{id}/answer", routing::post(answer))
        .route("/api/sessions/{id}/prewarm", routing::post(prewarm))
        .route("/api/sessions/{id}/seen", routing::post(seen))
        .route("/api/sessions/{id}/messages", routing::post(message))
        .route("/api/sessions/{id}/events", routing::get(events_ws))
        .route("/api/sessions/{id}/terminal", routing::get(terminal_ws))
}

#[cfg(test)]
mod tests;
