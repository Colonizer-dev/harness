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
}

impl ListQuery {
    /// Whether the query asks for a page at all: either field present changes the reply shape.
    fn paginated(&self) -> bool {
        self.limit.is_some() || self.cursor.is_some()
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
fn wrong_input(uhp: bool, headers: &HeaderMap, message: impl std::fmt::Display) -> Response {
    crate::uhp::error_for(uhp, headers, StatusCode::BAD_REQUEST, "invalid_input", message, None)
}

/// `GET /api/sessions`: the cockpit's bare array, newest first, unless the query asks for a page
/// (`limit`/`cursor`, issue #651) — then `{"sessions": […], "next_cursor": …}`, the §7 shape,
/// with the page's items alone decorated with their live activity.
pub async fn list(
    State(app): State<Shared>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Response {
    let visible = visible_sessions(&app, scoped.as_ref()).await;
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
enum MessageError {
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
struct Sent {
    id: String,
    delivered: bool,
}

/// The one user-message path (issue #746): the events socket's `user_message` command and
/// `POST /api/sessions/{id}/messages` both forward through here, so both check the colony, trim
/// and size-check the text, and mark a scoped token's message the same way. `client_id` is the
/// HTTP twin's dedupe key; the socket passes `None` and mints its own `u-<short_id>`.
async fn submit_message(
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
    if rt.commands.send(answer.forward(external)).is_err() {
        return Err(AnswerError::NotAccepting(s.status));
    }
    crate::activity::record_answer(app, &s, via).await;
    // The question is answered as far as the person is concerned (issue #744): every other
    // device closes its notification and drops the colony from its badge.
    spawn_resolved(app, id);
    Ok(())
}

/// The user message a resumed runner receives for a suspended colony's answer (issue #562): the
/// question as it was asked, then the choices made, then the free-text note. Pure, so tests pin the
/// wording the agent reads.
fn answer_prompt(questions: &[Value], answers: &Value, response: &str) -> String {
    let mut lines = vec![
        "Earlier you asked the user something, and this colony was suspended while it waited (its \
         microVM was stopped to free its slot). The conversation continues now — this is their answer."
            .to_string(),
    ];
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
            let note = {
                let sessions = app.sessions.read().await;
                crate::queue::restore_line_note(
                    &sessions,
                    &x,
                    orgs::global_max_parallel(&modules) as usize,
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

#[derive(Deserialize)]
pub struct SinceQuery {
    since: Option<u64>,
    epoch: Option<u64>,
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
    ws: WebSocketUpgrade,
) -> Result<Response, crate::AppError> {
    let revocation = revocation.map(|axum::Extension(r)| r);
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let rt = app.runtime(&id).await;
    let via = via.map(|axum::Extension(via)| via);
    let scoped = scoped.map(|axum::Extension(scoped)| scoped);
    let actor = SocketActor { via, scoped };
    // A revoked credential's socket closes at once (issue #746), not at its next request.
    Ok(ws.on_upgrade(move |socket| {
        crate::auth::Revocation::until(
            revocation,
            events_socket(app, id, rt, query.since.unwrap_or(0), query.epoch, actor, socket),
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

async fn events_socket(
    app: Shared,
    id: String,
    rt: Arc<Runtime>,
    since: u64,
    client_epoch: Option<u64>,
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
    let current_epoch = run_epoch_for_dir(&app.session_dir(&id));
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
    if let Ok(bytes) = tokio::fs::read(&rt.events_path).await {
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
                "skipped {} unreadable {} in {} during replay; the transcript continues past the gap",
                skipped,
                if skipped == 1 { "line" } else { "lines" },
                rt.events_path.display(),
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
        .route("/api/sessions/{id}/seen", routing::post(seen))
        .route("/api/sessions/{id}/messages", routing::post(message))
        .route("/api/sessions/{id}/events", routing::get(events_ws))
        .route("/api/sessions/{id}/terminal", routing::get(terminal_ws))
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::sessions::tests::*;

    /// Two colonies in one store, inserted oldest first so the newest-first order is visible.
    async fn two_colonies() -> (Shared, PathBuf) {
        let (app, root) = app_with_colony("older", SessionStatus::Running).await;
        let mut newer = colony("acme", SessionStatus::Idle);
        newer.id = "newer".into();
        app.sessions.write().await.push(newer);
        (app, root)
    }

    async fn list_response(app: &Shared, query: ListQuery) -> Value {
        let response = list(State(app.clone()), Query(query), HeaderMap::new(), None).await;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn page(limit: &str, cursor: Option<&str>) -> ListQuery {
        ListQuery {
            limit: Some(limit.into()),
            cursor: cursor.map(String::from),
        }
    }

    /// Without `limit` or `cursor` the route answers the cockpit's bare array, newest first —
    /// the shape the web UI and the sockets hub have always unwrapped.
    #[tokio::test]
    async fn the_list_stays_a_bare_array_without_pagination_params() {
        let (app, root) = two_colonies().await;
        let parsed = list_response(&app, ListQuery::default()).await;
        let ids: Vec<&str> = parsed.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["newer", "older"], "newest first, no wrapper: {parsed}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// With a pagination param the answer is the §7 page shape, and walking `next_cursor`
    /// reaches every colony exactly once and ends at `null`.
    #[tokio::test]
    async fn the_list_pages_newest_first_until_the_cursor_is_null() {
        let (app, root) = two_colonies().await;

        let first = list_response(&app, page("1", None)).await;
        assert_eq!(first["sessions"][0]["id"], "newer", "the page keeps the newest-first order");
        assert_eq!(first["next_cursor"], "newer", "the page's last colony is the next cursor");

        let second = list_response(&app, page("1", Some("newer"))).await;
        assert_eq!(
            second["sessions"][0]["id"], "older",
            "the cursor starts right after its colony"
        );
        assert_eq!(second["next_cursor"], Value::Null, "nothing follows the last colony");

        let last = list_response(&app, page("5", Some("older"))).await;
        assert_eq!(
            last["sessions"].as_array().unwrap().len(),
            0,
            "nothing follows the last colony"
        );
        assert_eq!(last["next_cursor"], Value::Null, "the walk ends at null");

        let both = list_response(&app, page("100", None)).await;
        let ids: Vec<&str> = both["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["newer", "older"], "one page holds them in order");
        assert_eq!(both["next_cursor"], Value::Null, "nothing follows a whole list");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A cursor that names no visible colony is a **400** `invalid_input`, not a page.
    #[tokio::test]
    async fn a_bad_cursor_is_invalid_input() {
        let (app, root) = two_colonies().await;
        for cursor in ["missing", "OLDER"] {
            let response = list(State(app.clone()), Query(page("1", Some(cursor))), HeaderMap::new(), None).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{cursor}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let parsed: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(parsed["code"], "invalid_input", "{cursor}: {parsed}");
        }
        // A malformed limit is the same refusal, naming its parameter.
        let response = list(State(app.clone()), Query(page("soon", None)), HeaderMap::new(), None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The page bounds are clamped, not refused: a zero limit reads one colony, an enormous one
    /// reads the whole list.
    #[tokio::test]
    async fn the_page_limit_is_clamped() {
        let (app, root) = two_colonies().await;
        let one = list_response(&app, page("0", None)).await;
        assert_eq!(one["sessions"].as_array().unwrap().len(), 1, "limit 0 clamps to 1");
        let all = list_response(&app, page("100000", None)).await;
        assert_eq!(
            all["sessions"].as_array().unwrap().len(),
            2,
            "limit past the list clamps to it"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A scoped token's org/repo limits filter the bare array and every page: outside them a
    /// colony is not in the answer at all, and its id is no cursor either.
    #[tokio::test]
    async fn the_scoped_filter_applies_to_bare_and_paged_alike() {
        let (app, root) = two_colonies().await;
        let mut elsewhere = colony("other", SessionStatus::Running);
        elsewhere.id = "elsewhere".into();
        elsewhere.repo = "other/repo".into();
        elsewhere.org = "other".into();
        app.sessions.write().await.push(elsewhere);

        let token = axum::Extension(crate::api_tokens::ScopedToken {
            id: "tok_test".into(),
            name: "watcher".into(),
            scope: crate::api_tokens::Scope::Read,
            orgs: Vec::new(),
            repos: vec!["acme/repo".into()],
            max_concurrent: None,
            budget_usd_per_day: None,
        });

        let bare = list_bare(State(app.clone()), Some(token.clone())).await;
        let ids: Vec<&str> = bare.0.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["newer", "older"], "the token sees only acme/repo: {ids:?}");

        let paged = list_response_with_token(&app, page("10", None), Some(token.clone())).await;
        let ids: Vec<&str> = paged["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["newer", "older"], "the page is filtered too: {paged}");
        assert_eq!(paged["next_cursor"], Value::Null);

        // A hidden colony's id is not a valid cursor either: it names no visible colony.
        let response = list(
            State(app.clone()),
            Query(page("1", Some("elsewhere"))),
            HeaderMap::new(),
            Some(token),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "a hidden id is no cursor");
        let _ = std::fs::remove_dir_all(root);
    }

    async fn list_response_with_token(
        app: &Shared,
        query: ListQuery,
        token: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
    ) -> Value {
        let response = list(State(app.clone()), Query(query), HeaderMap::new(), token).await;
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn commands_only_reach_a_colony_whose_microvm_is_up() {
        use SessionStatus::*;
        for status in [Starting, Running, WaitingForAnswer, Idle] {
            assert!(accepts_commands(status), "{status:?} should accept an answer");
        }
        // Publishing included: the microVM is already gone, and an answer sent then
        // used to vanish while the card kept spinning.
        for status in [Publishing, PrOpened, Merged, Closed, NoChanges, Stopped, Failed, Queued] {
            assert!(!accepts_commands(status), "{status:?} must not accept an answer");
        }
    }

    /// Issue #508: a `read`-scoped token may watch a colony's events socket but not drive it —
    /// its commands are refused with a warn on the transcript, never forwarded. An `operate`
    /// token's answer goes down to the agent with its free-text note marked as external input,
    /// its structured answers untouched, and the activity line names the token, never the secret.
    #[tokio::test]
    async fn a_read_scoped_token_watches_the_socket_but_cannot_drive_it() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let rt = app.runtime("abc").await;
        *rt.open_question.lock().await = Some(("q1".into(), Vec::new(), QuestionRisk::ReadOnly));
        let mut rx = rt.commands_rx.lock().await.take().unwrap();
        let scoped = |scope| crate::api_tokens::ScopedToken {
            id: "tok_test".into(),
            name: "watcher".into(),
            scope,
            orgs: Vec::new(),
            repos: Vec::new(),
            max_concurrent: None,
            budget_usd_per_day: None,
        };
        let answer = r#"{"type":"answer","question_id":"q1","answers":{"a":"b"},"response":"go"}"#;
        client_command(
            &app,
            "abc",
            &rt,
            Some(crate::auth::Via::Token("watcher".into())),
            Some(scoped(crate::api_tokens::Scope::Read)),
            answer,
        )
        .await;
        assert!(rx.try_recv().is_err(), "nothing was forwarded to the agent");
        {
            let logs = rt.logs.lock().await;
            let last = logs.back().unwrap();
            assert_eq!(last["level"], "warn", "{last}");
            assert!(
                last["message"]
                    .as_str()
                    .unwrap()
                    .contains("can watch this colony but not drive it"),
                "{last}"
            );
        }
        client_command(
            &app,
            "abc",
            &rt,
            Some(crate::auth::Via::Token("watcher".into())),
            Some(scoped(crate::api_tokens::Scope::Operate)),
            answer,
        )
        .await;
        let forwarded = rx.try_recv().unwrap();
        assert_eq!(forwarded["type"], "answer");
        assert_eq!(forwarded["question_id"], "q1");
        assert_eq!(forwarded["answers"], json!({"a": "b"}), "the answer labels are untouched");
        assert_eq!(
            forwarded["response"].as_str().unwrap(),
            "[external input from API token \"watcher\"] go",
            "the free-text note is marked as external input"
        );
        // The token's free-text message is external input too, marked like its answers; the
        // owner's message on the same socket is not.
        client_command(
            &app,
            "abc",
            &rt,
            Some(crate::auth::Via::Token("watcher".into())),
            Some(scoped(crate::api_tokens::Scope::Operate)),
            r#"{"type":"user_message","text":"rerun the failing suite"}"#,
        )
        .await;
        let forwarded = rx.try_recv().unwrap();
        assert_eq!(forwarded["type"], "user_message");
        assert_eq!(
            forwarded["text"].as_str().unwrap(),
            "[external input from API token \"watcher\"] rerun the failing suite",
            "the message is marked as external input"
        );
        client_command(&app, "abc", &rt, None, None, r#"{"type":"user_message","text":"carry on"}"#).await;
        let forwarded = rx.try_recv().unwrap();
        assert_eq!(forwarded["text"], "carry on", "the owner's message carries no marking");
        // Switching the model is not in the issue's operate list (issue #508): any scoped token is
        // refused with a warn on the transcript, never forwarded.
        client_command(
            &app,
            "abc",
            &rt,
            Some(crate::auth::Via::Token("watcher".into())),
            Some(scoped(crate::api_tokens::Scope::Launch)),
            r#"{"type":"set_model","model":"opus"}"#,
        )
        .await;
        assert!(rx.try_recv().is_err(), "set_model from a scoped token is not forwarded");
        {
            let logs = rt.logs.lock().await;
            let last = logs.back().unwrap();
            assert_eq!(last["level"], "warn", "{last}");
            assert!(
                last["message"].as_str().unwrap().contains("model"),
                "the warn says what was refused: {last}"
            );
        }
        let log = std::fs::read_to_string(app.cfg.data_dir.join(crate::activity::FILE)).unwrap();
        assert!(log.contains("token:watcher"), "the actor is the token: {log}");
        assert!(!log.contains("col_"), "no secret value reaches the log: {log}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #746: the messages twin shares the socket's path — the client id becomes the wire id
    /// (`u-<client>`), a repeat of the same id is answered without a second delivery, and the
    /// socket path (no client id) delivers every time.
    #[tokio::test]
    async fn the_message_twin_delivers_once_per_client_id() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let rt = app.runtime("abc").await;
        let mut rx = rt.commands_rx.lock().await.take().unwrap();

        let first = submit_message(&app, "abc", &rt, "  carry on  ", Some("deliv-1"), None)
            .await
            .unwrap();
        assert!(first.delivered);
        assert_eq!(first.id, "u-deliv-1", "the client id becomes the wire id");
        let forwarded = rx.try_recv().unwrap();
        assert_eq!(forwarded["type"], "user_message");
        assert_eq!(forwarded["id"], "u-deliv-1");
        assert_eq!(forwarded["text"], "carry on", "trimmed, like the socket path always trimmed");

        let again = submit_message(&app, "abc", &rt, "carry on", Some("deliv-1"), None)
            .await
            .unwrap();
        assert!(!again.delivered, "a repeated client id is a duplicate");
        assert_eq!(again.id, "u-deliv-1", "the reply still names the id");
        assert!(rx.try_recv().is_err(), "nothing was delivered twice");

        // A different client id delivers; so does a socket message with no client id at all.
        assert!(
            submit_message(&app, "abc", &rt, "go", Some("deliv-2"), None)
                .await
                .unwrap()
                .delivered
        );
        rx.try_recv().unwrap();
        let socket = submit_message(&app, "abc", &rt, "go", None, None).await.unwrap();
        assert!(socket.delivered);
        assert!(socket.id.starts_with("u-"), "the socket's id is minted: {}", socket.id);
        rx.try_recv().unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// A send into a dead runner channel (the receiver dropped while the colony stops) delivers
    /// nothing, so it must not be remembered: the twin answers the 409-shaped `Undelivered` and a
    /// retry of the same client id is not absorbed as a duplicate — the message is not silently
    /// lost.
    #[tokio::test]
    async fn a_failed_send_is_undelivered_and_leaves_the_client_id_free() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let rt = app.runtime("abc").await;
        drop(rt.commands_rx.lock().await.take().unwrap());

        assert!(
            matches!(
                submit_message(&app, "abc", &rt, "carry on", Some("retry-1"), None).await,
                Err(MessageError::Undelivered)
            ),
            "a send into a dead runner is an error, not a fake success"
        );
        // Not remembered: the same client id goes down the send path again (and fails again)
        // instead of being answered as a duplicate.
        assert!(matches!(
            submit_message(&app, "abc", &rt, "carry on", Some("retry-1"), None).await,
            Err(MessageError::Undelivered)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The answer twin's send into a dead runner answers the not-accepting 409 too, instead of a
    /// 204 for an answer that never reached the agent; the socket path discards the same error
    /// silently, as it always has.
    #[tokio::test]
    async fn a_failed_answer_send_is_not_a_204() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let rt = app.runtime("abc").await;
        *rt.open_question.lock().await = Some(("q1".into(), Vec::new(), QuestionRisk::ReadOnly));
        drop(rt.commands_rx.lock().await.take().unwrap());
        let parsed = AnswerCommand::parse(&json!({
            "type": "answer",
            "question_id": "q1",
            "answers": {"a": "b"},
            "response": "go",
        }))
        .unwrap();
        assert!(matches!(
            submit_answer(&app, "abc", &rt, parsed, None, None, true).await,
            Err(AnswerError::NotAccepting(_))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The twin marks a scoped token's text as external input like the socket does, and refuses
    /// with a named error what the socket drops silently — without burning the refused message's
    /// client id, so a corrected retry still delivers.
    #[tokio::test]
    async fn the_message_twin_marks_a_scoped_tokens_text_and_refuses_the_rest() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let rt = app.runtime("abc").await;
        let mut rx = rt.commands_rx.lock().await.take().unwrap();
        let scoped = crate::api_tokens::ScopedToken {
            id: "tok_test".into(),
            name: "watcher".into(),
            scope: crate::api_tokens::Scope::Operate,
            orgs: Vec::new(),
            repos: Vec::new(),
            max_concurrent: None,
            budget_usd_per_day: None,
        };
        let sent = submit_message(&app, "abc", &rt, "rerun the suite", Some("watch-1"), Some(&scoped))
            .await
            .unwrap();
        assert_eq!(sent.id, "u-watch-1");
        assert_eq!(
            rx.try_recv().unwrap()["text"].as_str().unwrap(),
            "[external input from API token \"watcher\"] rerun the suite",
            "the message is marked as external input"
        );

        assert!(matches!(
            submit_message(&app, "abc", &rt, "   ", Some("watch-2"), None).await,
            Err(MessageError::Invalid)
        ));
        assert!(matches!(
            submit_message(&app, "abc", &rt, &"x".repeat(100_001), Some("watch-3"), None).await,
            Err(MessageError::Invalid)
        ));
        assert!(matches!(
            submit_message(&app, "zzz", &rt, "hello", Some("watch-4"), None).await,
            Err(MessageError::NoSession)
        ));
        assert!(
            submit_message(&app, "abc", &rt, "now really", Some("watch-2"), None)
                .await
                .unwrap()
                .delivered,
            "an invalid text did not burn its client id"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A colony whose microVM is down refuses a message naming its state — the twin's 409, where
    /// the socket answers a stale client with a fresh session frame instead.
    #[tokio::test]
    async fn the_message_twin_refuses_a_colony_that_cannot_take_one() {
        let (app, root) = app_with_colony("abc", SessionStatus::Merged).await;
        let rt = app.runtime("abc").await;
        assert!(matches!(
            submit_message(&app, "abc", &rt, "hello", Some("late-1"), None).await,
            Err(MessageError::NotAccepting(SessionStatus::Merged))
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #746: the offline outbox replays a queued answer with the question content it saw.
    /// The replay delivers once — a second replay finds the question closed — and an answer to a
    /// question that changed under the same id (a rebooted runner counts `q-1` afresh) is refused
    /// as stale instead of answering something the operator never read.
    #[tokio::test]
    async fn a_replayed_answer_delivers_once_and_never_to_a_changed_question() {
        let (app, root) = app_with_colony("abc", SessionStatus::WaitingForAnswer).await;
        let rt = app.runtime("abc").await;
        let mut rx = rt.commands_rx.lock().await.take().unwrap();
        let asked = vec![json!({"question": "Push now?", "options": [{"label": "yes"}, {"label": "no"}]})];
        let replay = |saw: &Vec<Value>| {
            AnswerCommand::parse(&json!({
                "question_id": "q-1",
                "answers": {"Push now?": "yes"},
                "questions": saw,
            }))
            .unwrap()
        };

        // The question changed under the same id while the answer sat in the queue: refused.
        let changed = vec![json!({"question": "Delete the branch?", "options": [{"label": "yes"}]})];
        *rt.open_question.lock().await = Some(("q-1".into(), changed, QuestionRisk::WorkspaceWrite));
        assert!(matches!(
            submit_answer(&app, "abc", &rt, replay(&asked), None, None, true).await,
            Err(AnswerError::Stale)
        ));
        assert!(rx.try_recv().is_err(), "nothing reached the agent");

        // The question it saw: delivered, once.
        *rt.open_question.lock().await = Some(("q-1".into(), asked.clone(), QuestionRisk::WorkspaceWrite));
        assert!(
            submit_answer(&app, "abc", &rt, replay(&asked), None, None, true)
                .await
                .is_ok()
        );
        let forwarded = rx.try_recv().unwrap();
        assert_eq!(forwarded["question_id"], "q-1");
        assert!(
            forwarded.get("questions").is_none(),
            "the runner gets the answer, not the check"
        );
        assert!(matches!(
            submit_answer(&app, "abc", &rt, replay(&asked), None, None, true).await,
            Err(AnswerError::NoQuestion)
        ));
        assert!(rx.try_recv().is_err(), "a second replay delivers nothing");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn client_ids_are_bounded_and_shaped() {
        assert!(valid_client_id("m-1_2"));
        assert!(valid_client_id(&"a".repeat(64)));
        assert!(!valid_client_id(&"a".repeat(65)), "64 is the cap");
        assert!(!valid_client_id(""));
        assert!(!valid_client_id("has space"));
        assert!(!valid_client_id("unicode-é"));
        assert!(!valid_client_id("../escape"));
    }

    /// Issue #508: the question route separates an empty inbox from an unknown colony — a colony
    /// that is not asking reads 204 with no body (nothing to answer, not an error), an unknown id
    /// stays a 404, and an open question reads as the body `colonizer ask` and the cockpit parse.
    #[tokio::test]
    async fn the_question_route_answers_nothing_pending_with_a_204() {
        use axum::body::to_bytes;

        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;

        // Nothing pending: 204, no body.
        let response = question(State(app.clone()), Path("abc".into())).await.unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // An open question reads as the JSON body, carrying the id an answer names.
        let rt = app.runtime("abc").await;
        *rt.open_question.lock().await = Some((
            "q1".into(),
            vec![json!({"question": "Push now?", "options": []})],
            QuestionRisk::WorkspaceWrite,
        ));
        let response = question(State(app.clone()), Path("abc".into())).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["question_id"], "q1");

        // An unknown colony stays a 404.
        let error = question(State(app), Path("zzz".into())).await.unwrap_err();
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }

    /// `POST /api/sessions/{id}/seen` (issue #744): a look at a colony with nothing unseen answers
    /// 204 and stays silent — its question, if any, is still open, and no other device may close a
    /// notification for it — while a look at an unseen failure clears the flag and resolves it on
    /// the one subscribed device. An unknown colony is a 404 either way.
    #[tokio::test]
    async fn seen_clears_the_unseen_failure_and_404s_an_unknown_id() {
        let captures: crate::push::tests::Captures = std::sync::Arc::default();
        let addr = crate::push::tests::capture_server(captures.clone()).await;
        let (app, root) = app_with_colony("abc", SessionStatus::Failed).await;
        let phone = crate::push::tests::device(&format!("http://{addr}/phone"), [7u8; 16]);
        crate::push::tests::notify_on(&app).await;
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        crate::push::save(&app.cfg.config_dir, &[phone.subscription]).unwrap();

        let response = seen(State(app.clone()), Path("abc".into())).await.unwrap();
        assert_eq!(response, StatusCode::NO_CONTENT);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(captures.lock().unwrap().is_empty(), "a look with nothing unseen stays silent");

        app.update_session("abc", |s| s.unseen_failure = true).await.unwrap();
        assert!(
            crate::push::needs_you(&app.session("abc").await.unwrap()),
            "the failure is unseen"
        );
        let response = seen(State(app.clone()), Path("abc".into())).await.unwrap();
        assert_eq!(response, StatusCode::NO_CONTENT);
        crate::push::tests::await_captures(&captures, 1).await;
        assert!(!app.session("abc").await.unwrap().unseen_failure, "the failure has been seen");

        let error = seen(State(app), Path("zzz".into())).await.unwrap_err();
        assert_eq!(error.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn set_model_forwards_only_what_a_model_setting_could_hold() {
        for id in [
            "opus",
            "claude-opus-5-5",
            "claude-opus-5-5[1m]",
            "deepseek/deepseek-flash",
            "together/deepseek-ai/DeepSeek-V4.1-Flash",
            "local/qwen3:32b",
        ] {
            assert_eq!(set_model_id(id), Some(id), "{id}");
        }
        assert_eq!(set_model_id("  sonnet\n"), Some("sonnet"), "trimmed");
        assert!(set_model_id(&"m".repeat(128)).is_some());
        // No model id has whitespace, quotes or non-ASCII in it.
        for bad in ["", "   ", "has space", "opus\"}", "opus\n{\"type\":\"shutdown\"}", "claudé"] {
            assert_eq!(set_model_id(bad), None, "{bad:?}");
        }
        // The longest id `/api/models` can list: a 32-byte provider id, `/`, a 120-byte model.
        let longest = format!("{}/{}", "p".repeat(32), "m".repeat(120));
        assert_eq!(set_model_id(&longest), Some(longest.as_str()), "the longest listed id");
        assert_eq!(set_model_id(&format!("{longest}m")), None, "one byte over");
    }

    /// The user message a resumed runner receives (issue #562): every question as it was asked,
    /// the choices made (a multi-select joined), a question with no choice said so, and the
    /// free-text note trimmed in. Wording the agent reads — pinned.
    #[test]
    fn the_answer_prompt_replays_the_questions_the_choices_and_the_note() {
        let questions = vec![
            json!({"question": "Which file name?", "options": [{"label": "hello.txt"}]}),
            json!({"question": "Which extras?", "options": []}),
            json!({"question": "Ship it?"}),
        ];
        let answers = json!({"Which file name?": "hello.txt", "Which extras?": ["lint", "format"]});
        let prompt = answer_prompt(&questions, &answers, "  make it quick  ");
        assert!(prompt.starts_with("Earlier you asked the user something"), "{prompt}");
        assert!(prompt.contains("Q: Which file name?\nA: hello.txt"), "{prompt}");
        assert!(prompt.contains("Q: Which extras?\nA: lint, format"), "{prompt}");
        assert!(prompt.contains("Q: Ship it?\nA: (no choice given)"), "{prompt}");
        assert!(prompt.ends_with("Their note: make it quick"), "{prompt}");
        let quiet = answer_prompt(&questions, &answers, "   ");
        assert!(!quiet.contains("Their note"), "{quiet}");
    }

    /// An answer to a suspended colony (issue #562) goes nowhere — there is no runner — but must
    /// never be lost: it is held on the record and persisted before the answer returns, the open
    /// question is closed in the event log so a restart does not re-open it, the colony keeps its
    /// status and its suspension, and a second answer finds no question to answer.
    #[tokio::test]
    async fn an_answer_to_a_suspended_colony_is_kept_and_the_question_closed() {
        let (app, root) = app_with_colony("abc", SessionStatus::WaitingForAnswer).await;
        let rt = app.runtime("abc").await;
        let mut rx = rt.commands_rx.lock().await.take().unwrap();
        let questions = vec![json!({
            "question": "Which file name?",
            "header": "File",
            "options": [{"label": "hello.txt"}, {"label": "hi.txt"}],
        })];
        *rt.open_question.lock().await = Some(("q1".into(), questions, QuestionRisk::ReadOnly));
        rt.activity.lock().await.question_since = Some(Utc::now());
        app.update_session("abc", |x| {
            x.suspended = Some(Suspension {
                at: Utc::now(),
                snapshot: None,
                reason: WAITING_FOR_ANSWER.into(),
                path: SESSION_RESUME.into(),
            });
        })
        .await
        .unwrap();

        let answer = AnswerCommand {
            question_id: "q1".into(),
            answers: json!({"Which file name?": "hello.txt"}),
            response: Value::String("go ahead".into()),
            questions: None,
        };
        assert!(
            submit_answer(&app, "abc", &rt, answer, None, None, true).await.is_ok(),
            "a suspended colony takes answers"
        );

        let s = app.session("abc").await.unwrap();
        let held = s.pending_answer.as_ref().expect("the answer is held on the record");
        assert_eq!(held.question_id, "q1");
        assert!(held.prompt.contains("Q: Which file name?\nA: hello.txt"), "{}", held.prompt);
        assert!(held.prompt.ends_with("Their note: go ahead"), "{}", held.prompt);
        assert_eq!(
            s.status,
            SessionStatus::WaitingForAnswer,
            "the status is untouched — the colony is still waiting, now with the answer"
        );
        assert!(
            s.suspended.is_some(),
            "and still suspended: nothing answered the question yet"
        );
        assert!(
            std::fs::read_to_string(app.sessions_file())
                .unwrap()
                .contains("pending_answer"),
            "the held answer reaches sessions.json before the answer returns"
        );
        let events = std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap();
        assert!(
            events.contains("\"question_answered\"") && events.contains("\"q1\""),
            "the question is closed in the event log so a restart's replay does not re-open it: {events}"
        );
        assert!(rt.open_question.try_lock().unwrap().is_none(), "no question is open any more");
        assert!(rt.activity.lock().await.question_since.is_none());
        assert!(
            rx.try_recv().is_err(),
            "nothing is sent down the link — there is no runner to receive it"
        );
        {
            let logs = rt.logs.lock().await;
            let last = logs.back().unwrap();
            assert!(last["message"].as_str().unwrap().contains("kept on the colony"), "{last}");
        }
        // A second answer finds no open question: the first was taken.
        let again = AnswerCommand {
            question_id: "q1".into(),
            answers: json!({}),
            response: Value::Null,
            questions: None,
        };
        assert!(
            matches!(
                submit_answer(&app, "abc", &rt, again, None, None, true).await,
                Err(AnswerError::NoQuestion)
            ),
            "the held answer took the question with it"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The race the suspension tick and a live answer could make (issue #562), closed by the open
    /// question's lock both take: an answer that forwarded to the live runner takes the question
    /// down as it goes, so the tick reads no question and leaves the colony running with its
    /// answer in flight — and a tick that claimed first has the answer held instead, with nothing
    /// sent down a link that teardown is stopping.
    #[tokio::test]
    async fn an_answer_and_the_suspension_claim_cannot_interleave() {
        /// A waiting, resumable colony with the question `q1` open and past the grace, and the
        /// receiver end of its agent link's command channel.
        async fn asked(app: &Shared, id: &str) -> tokio::sync::mpsc::UnboundedReceiver<Value> {
            let mut s = colony("acme", SessionStatus::WaitingForAnswer);
            s.id = id.into();
            s.agent = "claude-code".into();
            s.agent_session = Some("s1".into());
            app.sessions.write().await.push(s);
            std::fs::create_dir_all(app.session_dir(id)).unwrap();
            let rt = app.runtime(id).await;
            rt.activity.lock().await.question_since = Some(Utc::now() - chrono::Duration::minutes(20));
            *rt.open_question.lock().await = Some((
                "q1".into(),
                vec![json!({"question": "Push now?", "options": []})],
                QuestionRisk::WorkspaceWrite,
            ));
            rt.commands_rx.lock().await.take().unwrap()
        }
        let answer = || AnswerCommand {
            question_id: "q1".into(),
            answers: json!({"Push now?": "yes"}),
            response: Value::Null,
            questions: None,
        };

        let root = std::env::temp_dir().join(format!("colonizer-answer-race-{}", crate::util::short_id()));
        let app = crate::tests::test_app_with_agents(
            &root,
            vec![
                crate::modules::AgentModule::test("claude-code")
                    .dir(std::path::PathBuf::from("/opt/colonizer/agent"))
                    .entry(vec!["runner.mjs".into()])
                    .resume_dir(Some("/root/.claude/projects".into())),
            ],
            |_| {},
        );
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("suspend_waiting".into(), json!(true));
        app.modules
            .write()
            .await
            .sandbox
            .settings
            .insert("suspend_after_minutes".into(), json!(1));
        let modules = app.modules.read().await.clone();
        let mut live = asked(&app, "answered-first").await;
        let mut claimed = asked(&app, "claimed-first").await;

        // The answer wins the race: it is forwarded to the runner that is still up, and takes the
        // open question down with it.
        assert!(
            submit_answer(
                &app,
                "answered-first",
                &app.runtime("answered-first").await,
                answer(),
                None,
                None,
                true
            )
            .await
            .is_ok()
        );
        let forwarded = live.try_recv().unwrap();
        assert_eq!(forwarded["type"], "answer", "the answer went down the live link");
        assert_eq!(forwarded["question_id"], "q1");
        assert!(
            app.runtime("answered-first")
                .await
                .open_question
                .try_lock()
                .unwrap()
                .is_none(),
            "the question went with the answer, as the runner's own echo would take it"
        );

        // So the tick skips this colony: an answer is in flight to a runner it must not tear down.
        crate::queue::suspend_waiting_colonies(&app, &modules).await;
        let s = app.session("answered-first").await.unwrap();
        assert!(
            s.suspended.is_none() && s.holds_slot(),
            "the colony keeps its microVM and its slot — its answer is on its way"
        );
        assert_eq!(
            s.status,
            SessionStatus::WaitingForAnswer,
            "the runner has not echoed yet; its status events take it from here"
        );
        assert!(s.pending_answer.is_none(), "nothing was held — it was delivered");

        // The claim wins the other race: the same tick suspended the colony still waiting, and an
        // answer arriving after that is held, not sent into the link the teardown is stopping.
        let s = app.session("claimed-first").await.unwrap();
        assert!(
            s.suspended.is_some() && !s.holds_slot(),
            "the unanswered colony was suspended"
        );
        assert!(
            submit_answer(
                &app,
                "claimed-first",
                &app.runtime("claimed-first").await,
                answer(),
                None,
                None,
                true
            )
            .await
            .is_ok()
        );
        assert!(
            claimed.try_recv().is_err(),
            "nothing goes down a link the teardown is stopping"
        );
        let s = app.session("claimed-first").await.unwrap();
        let held = s.pending_answer.as_ref().expect("the answer is held for the restore");
        assert_eq!(held.question_id, "q1");
        assert!(s.suspended.is_some(), "still suspended, the restore to deliver it");
        let _ = std::fs::remove_dir_all(root);
    }
}
