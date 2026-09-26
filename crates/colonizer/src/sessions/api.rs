//! The session HTTP handlers: list, detail, the open question and its answer, and the events and
//! terminal WebSockets.

use super::*;

pub async fn list(
    State(app): State<Shared>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Json<Vec<Session>> {
    let sessions = app.sessions.read().await.clone();
    let mut out = Vec::with_capacity(sessions.len());
    for session in sessions.into_iter().rev() {
        // A scoped token's org/repo limits are also the list filter (issue #508): a colony outside
        // them is not in the answer at all, the same hiding a single-colony read gets.
        if let Some(token) = &scoped
            && !token.covers(&session.org, &session.repo)
        {
            continue;
        }
        out.push(with_activity(&app, session).await);
    }
    Json(out)
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

/// An answer as the API takes it: the question it answers, one choice per question label, and the
/// free-text note the agent reads alongside. Exactly the socket command's fields (`client_command`),
/// parsed once so both paths validate identically.
struct AnswerCommand {
    question_id: String,
    answers: Value,
    response: Value,
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
enum AnswerError {
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
async fn submit_answer(
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
        let open_matches = open.as_ref().is_some_and(|(open_id, ..)| open_id == &answer.question_id);
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
    // The answer is on its way to the runner, whose own `question_answered` echo closes the
    // question: close it here first, so the tick — which claims only under an open question, and
    // matching the one the answer is for — reads none. A stale answer (socket path) matches
    // nothing and leaves the live question standing for the runner to refuse it.
    if gate.as_ref().is_some_and(|(open_id, ..)| open_id == &answer.question_id) {
        *gate = None;
    }
    drop(gate);
    let _ = rt.commands.send(answer.forward(external));
    crate::activity::record_answer(app, &s, via).await;
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
                });
            }
            was
        })
        .await;
    match stored {
        Some((_, true)) => {
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
            rt.activity.lock().await.question_since = None;
            if let Some(s) = app.session(id).await {
                crate::activity::record_answer(app, &s, via).await;
            }
            app.session_log(
                id,
                "info",
                "answer received while suspended; it is kept on the colony and delivered when it resumes".into(),
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
    ws: WebSocketUpgrade,
) -> Result<Response, crate::AppError> {
    app.session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let rt = app.runtime(&id).await;
    let via = via.map(|axum::Extension(via)| via);
    let scoped = scoped.map(|axum::Extension(scoped)| scoped);
    let actor = SocketActor { via, scoped };
    Ok(ws.on_upgrade(move |socket| events_socket(app, id, rt, query.since.unwrap_or(0), query.epoch, actor, socket)))
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
            let text = command["text"].as_str().unwrap_or_default().trim();
            if text.is_empty() || text.len() > 100_000 {
                return;
            }
            // A scoped token's message is external input like its answers (issue #508): the
            // marker tells the agent who is talking, in the operator's voice or not.
            let text = match &scoped {
                Some(token) => external_text(&token.name, text),
                None => text.to_string(),
            };
            json!({"type": "user_message", "id": format!("u-{}", short_id()), "text": text})
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
    ws: WebSocketUpgrade,
) -> Result<Response, crate::AppError> {
    let s = app
        .session(&id)
        .await
        .ok_or_else(|| client_error(StatusCode::NOT_FOUND, "no such session"))?;
    let cols = query.cols.unwrap_or(80).clamp(10, 500);
    let rows = query.rows.unwrap_or(24).clamp(5, 300);
    Ok(ws.on_upgrade(move |socket| terminal_socket(app, s, cols, rows, socket)))
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
    axum::Router::new()
        .route("/api/sessions", routing::get(list).post(create))
        .route("/api/sessions/{id}", routing::get(get))
        .route("/api/sessions/{id}/question", routing::get(question))
        .route("/api/sessions/{id}/answer", routing::post(answer))
        .route("/api/sessions/{id}/events", routing::get(events_ws))
        .route("/api/sessions/{id}/terminal", routing::get(terminal_ws))
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::sessions::tests::*;

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
        };

        let root = std::env::temp_dir().join(format!("colonizer-answer-race-{}", crate::util::short_id()));
        let app = crate::tests::test_app_with_agents(
            &root,
            vec![crate::modules::AgentModule {
                id: "claude-code".into(),
                name: "claude-code".into(),
                description: String::new(),
                dir: std::path::PathBuf::from("/opt/colonizer/agent"),
                entry: vec!["runner.mjs".into()],
                needs_claude: false,
                schema: json!({}),
                egress: None,
                resume_dir: Some("/root/.claude/projects".into()),
            }],
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
