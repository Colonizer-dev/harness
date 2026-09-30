//! Answering a colony's question straight from its push notification (issue #742): the question
//! push carries a one-shot token and the question's option labels, and `POST /api/push/answer`
//! turns a tap on the notification into the same answer the cockpit's question card sends.
//!
//! [`Registry::for_push`] mints the token as `push::deliver` builds the payload and keeps it here
//! against its SHA-256 only. The route authenticates by the body's token alone — `host_guard`
//! exempts exactly this method+path from the cookie/bearer wall, the Host allowlist still applies —
//! so the content of a leaked notification is a credential for one answer to one question, for
//! [`TTL`], and nothing else. The answer rides the shared answer path
//! ([`crate::sessions::submit_answer`]), so the open question's gate, the suspension hold
//! (issue #562) and the activity line are the cockpit's own. Like the other pushes, the payload
//! never carries the question's text or header — labels only.
//!
//! The registry is in memory: a mothership restart drops outstanding tokens and the notification's
//! buttons answer 401 — the cockpit's question card is the way back in.

use std::{collections::HashMap, time::Duration, time::Instant};

use axum::{extract::State, http::StatusCode, response::Json};
use serde_json::{Value, json};

use crate::{App, Shared, client_error};

/// How long a push's answer token works: a notification read the next morning still answers, if
/// its question is still the colony's open one.
pub(crate) const TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// The most labels a notification offers and a token accepts — the same cap `for_push` puts on
/// the payload, and what a notification shade has room to show.
const MAX_CHOICES: usize = 3;
/// The most free text an "Other" answer carries — far above any real note, but a bound beats an
/// unbounded string into the transcript.
const MAX_OTHER: usize = 2000;

/// SHA-256 of a token, hex, as `api_tokens` stores its own: the registry never holds a plaintext
/// token, so neither the process's memory beyond this map nor a dump of it reads as a link.
fn hash_token(token: &str) -> String {
    crate::util::hex(ring::digest::digest(&ring::digest::SHA256, token.as_bytes()).as_ref())
}

/// SHA-256 of the question set as the colony asked it, hex. Question ids alone do not name one
/// question for life — most runners number them `q-1`, `q-2`… from a counter that starts over when
/// a suspended colony boots again — so a token also pins the exact questions it was minted for.
fn question_digest(questions: &[Value]) -> String {
    let text = serde_json::to_string(questions).unwrap_or_default();
    crate::util::hex(ring::digest::digest(&ring::digest::SHA256, text.as_bytes()).as_ref())
}

/// One minted token's claims: the colony and question it answers, a digest of that question's
/// content, the labels the payload offered and when it stops working. Session, id and digest are
/// checked against the runtime at answer time, so a question that closed or was replaced — even by
/// one reusing its id — refuses inside the TTL.
#[derive(Clone)]
struct Entry {
    session: String,
    question_id: String,
    digest: String,
    labels: Vec<String>,
    expires_at: Instant,
}

/// The answer tokens of the running process, keyed by the token's SHA-256.
#[derive(Default)]
pub struct Registry {
    entries: tokio::sync::Mutex<HashMap<String, Entry>>,
}

impl Registry {
    /// The token and labels a question push should carry, if its question can be answered from a
    /// notification at all: exactly one question, not a multi-select, and one to three labels.
    /// One token per push — every subscribed device shows the same buttons, and the first tap
    /// anywhere wins.
    pub(crate) async fn for_push(&self, app: &App, session: &str) -> Option<(String, Vec<String>)> {
        let (question_id, questions, _) = open_question(app, session).await?;
        let [question] = questions.as_slice() else {
            return None;
        };
        if question["multi_select"].as_bool().unwrap_or(false) {
            return None;
        }
        let labels: Vec<String> = question["options"]
            .as_array()
            .map(|options| {
                options
                    .iter()
                    .filter_map(|o| o["label"].as_str())
                    .filter(|label| !label.trim().is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if labels.is_empty() || labels.len() > MAX_CHOICES {
            return None;
        }
        let token = crate::util::random_token();
        self.insert(
            app,
            &token,
            Entry {
                session: session.to_string(),
                question_id,
                digest: question_digest(&questions),
                labels: labels.clone(),
                expires_at: Instant::now() + TTL,
            },
        )
        .await;
        Some((token, labels))
    }

    /// Stores a minted token after pruning what can no longer answer: expired entries, the same
    /// colony's previous token (each push replaces the last) and entries whose question is no
    /// longer their colony's open one. `expires_at` rides on the entry, so a test can mint a dead
    /// one.
    async fn insert(&self, app: &App, token: &str, entry: Entry) {
        let now = Instant::now();
        // The closed-question check reads the runtimes map and each runtime's question lock, so it
        // runs over a snapshot with this registry's lock released — never one lock held across the
        // others — and the dead keys are dropped under a second short lock.
        let snapshot: Vec<(String, Entry)> = self
            .entries
            .lock()
            .await
            .iter()
            .map(|(key, e)| (key.clone(), e.clone()))
            .collect();
        let mut dead = Vec::new();
        for (key, e) in snapshot {
            if e.expires_at <= now || e.session == entry.session || !still_open(app, &e).await {
                dead.push(key);
            }
        }
        let mut entries = self.entries.lock().await;
        for key in dead {
            entries.remove(&key);
        }
        entries.insert(hash_token(token), entry);
    }

    /// Drops every token for one colony: its question was answered or replaced (events.rs calls
    /// this on `question` and `question_answered`), so no notification of the old one answers.
    pub(crate) async fn revoke(&self, session: &str) {
        self.entries.lock().await.retain(|_, e| e.session != session);
    }

    /// A live token's claims, without consuming it — the checks run against this, then [`Registry::take`]
    /// spends the token, so a body that fails a check leaves the link exactly as it was.
    async fn peek(&self, token: &str) -> Option<Entry> {
        self.entries.lock().await.get(&hash_token(token)).cloned()
    }

    /// Consumes a token: single use, first tap wins — the remove is atomic, so two devices tapping
    /// at once both see a live token and exactly one of them gets it.
    async fn take(&self, token: &str) -> Option<Entry> {
        self.entries.lock().await.remove(&hash_token(token))
    }
}

/// Whether an entry's question is still its colony's open one: the same id and the same content.
async fn still_open(app: &App, entry: &Entry) -> bool {
    open_question(app, &entry.session)
        .await
        .is_some_and(|(id, questions, _)| id == entry.question_id && question_digest(&questions) == entry.digest)
}

/// The route this module serves. `server::api_routes` merges it in; `host_guard` admits this one
/// method+path without cookie or bearer, because the body's token is the credential.
pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing;
    axum::Router::new().route("/api/push/answer", routing::post(answer))
}

/// The colony's open question, read-only — like notify's `open_question_id`, without creating a
/// runtime for a colony that has none.
async fn open_question(app: &App, session: &str) -> Option<(String, Vec<Value>, crate::protocol::QuestionRisk)> {
    let rt = app.runtimes.lock().await.get(session).cloned()?;
    rt.open_question.lock().await.as_ref().cloned()
}

/// What `POST /api/push/answer` takes: the token from the payload plus exactly one of a chosen
/// label or a free-text note.
struct AnswerBody {
    token: String,
    answered: Answered,
}

enum Answered {
    Choice(String),
    Other(String),
}

impl AnswerBody {
    /// `None` for a body that is not one answer: no token, neither `choice` nor `other`, both, or
    /// an over-long note. Whitespace means absent, so `{"choice": "  "}` is not an answer.
    fn parse(command: &Value) -> Option<AnswerBody> {
        let token = command["token"].as_str()?.trim();
        if token.is_empty() {
            return None;
        }
        let choice = command["choice"].as_str().map(str::trim).filter(|c| !c.is_empty());
        let other = command["other"].as_str().map(str::trim).filter(|o| !o.is_empty());
        let answered = match (choice, other) {
            (Some(choice), None) => Answered::Choice(choice.to_string()),
            (None, Some(other)) if other.chars().count() <= MAX_OTHER => Answered::Other(other.to_string()),
            _ => return None,
        };
        Some(AnswerBody {
            token: token.to_string(),
            answered,
        })
    }
}

/// The notification's tap, as an answer. The token decides: unknown, spent or expired is a 401, a
/// choice the payload never offered is a 400, and a question that closed or moved on is a 409.
pub async fn answer(State(app): State<Shared>, Json(command): Json<Value>) -> Result<Json<Value>, crate::AppError> {
    let Some(body) = AnswerBody::parse(&command) else {
        return Err(client_error(
            StatusCode::BAD_REQUEST,
            "expected {\"token\": str, \"choice\": str} or {\"token\": str, \"other\": str} — exactly one of the two",
        ));
    };
    let stale = "the colony is no longer asking this question; open it in the cockpit to see what it asks now";
    let Some(entry) = app.answer_tokens.peek(&body.token).await else {
        return Err(client_error(
            StatusCode::UNAUTHORIZED,
            "this answer link is not valid any more",
        ));
    };
    if entry.expires_at <= Instant::now() {
        app.answer_tokens.take(&body.token).await;
        return Err(client_error(StatusCode::UNAUTHORIZED, "this answer link has expired"));
    }
    let text = match &body.answered {
        Answered::Choice(choice) if entry.labels.iter().any(|l| l == choice) => choice.clone(),
        Answered::Choice(_) => {
            return Err(client_error(
                StatusCode::BAD_REQUEST,
                "that is not one of the choices the notification offered",
            ));
        }
        Answered::Other(note) => note.clone(),
    };
    // The answer goes into the answers map under the question's own text — the same shape the
    // cockpit's question card sends, so the shared path and the runner cannot tell them apart. A
    // question that closed or moved on (a new one, or the same id with other content) burns the
    // token: it can never answer again.
    let open = open_question(&app, &entry.session).await;
    let question = match &open {
        Some((open_id, questions, _)) if open_id == &entry.question_id && question_digest(questions) == entry.digest => {
            questions.first().and_then(|q| q["question"].as_str())
        }
        _ => None,
    };
    let Some(question) = question else {
        app.answer_tokens.take(&body.token).await;
        return Err(client_error(StatusCode::CONFLICT, stale));
    };
    let mut answers = serde_json::Map::new();
    answers.insert(question.to_string(), Value::String(text.clone()));
    let command = crate::sessions::AnswerCommand {
        question_id: entry.question_id.clone(),
        answers: answers.into(),
        response: Value::Null,
    };
    // Then consume, and only then submit: a tap that lost the race stops here as a 401, and the
    // colony takes exactly one answer.
    let Some(entry) = app.answer_tokens.take(&body.token).await else {
        return Err(client_error(StatusCode::UNAUTHORIZED, "another device answered first"));
    };
    let rt = app.runtime(&entry.session).await;
    match crate::sessions::submit_answer(&app, &entry.session, &rt, command, None, None, true).await {
        Ok(()) => Ok(Json(json!({ "answered": text }))),
        Err(crate::sessions::AnswerError::NoSession) => Err(client_error(StatusCode::NOT_FOUND, "no such session")),
        Err(crate::sessions::AnswerError::NotAccepting(status)) => Err(client_error(
            StatusCode::CONFLICT,
            &format!("the colony is {} and cannot take an answer; resume it first", status.as_str()),
        )),
        Err(crate::sessions::AnswerError::NoQuestion | crate::sessions::AnswerError::Stale) => {
            Err(client_error(StatusCode::CONFLICT, stale))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::tests::*;
    use axum::{
        Router,
        http::{Request, header},
    };
    use tower::ServiceExt as _;

    /// A waiting colony with the question `q1` open — "Push now?", header "File", two labels — and
    /// the receiver end of its agent link's command channel.
    async fn asking() -> (Shared, tokio::sync::mpsc::UnboundedReceiver<Value>, std::path::PathBuf) {
        let (app, root) = app_with_colony("abc", crate::sessions::SessionStatus::WaitingForAnswer).await;
        let rt = app.runtime("abc").await;
        *rt.open_question.lock().await = Some((
            "q1".into(),
            vec![json!({
                "question": "Push now?",
                "header": "File",
                "options": [{"label": "Push now"}, {"label": "Wait"}],
            })],
            crate::protocol::QuestionRisk::ReadOnly,
        ));
        let rx = rt.commands_rx.lock().await.take().unwrap();
        (app, rx, root)
    }

    /// A minted token for `asking`'s question: labels as the push would carry them.
    async fn mint(app: &Shared) -> String {
        app.answer_tokens.for_push(app, "abc").await.unwrap().0
    }

    /// The real `host_guard` over the two answer routes, driven with `oneshot`.
    fn router(app: &Shared) -> Router<()> {
        Router::new()
            .route("/api/push/answer", axum::routing::post(answer))
            .route("/api/sessions/{id}/answer", axum::routing::post(crate::sessions::answer))
            .layer(axum::middleware::from_fn_with_state(app.clone(), crate::server::host_guard))
            .with_state(app.clone())
    }

    /// A request through the guard: loopback Host, a JSON body, no credential of any kind.
    fn post(uri: &str, body: &str) -> Request<axum::body::Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7878")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    }

    async fn status_and_json(res: axum::response::Response) -> (StatusCode, Value) {
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// A push answer through the guard, as (status, body).
    async fn tap(app: &Shared, body: String) -> (StatusCode, Value) {
        status_and_json(router(app).oneshot(post("/api/push/answer", &body)).await.unwrap()).await
    }

    #[tokio::test]
    async fn a_tap_answers_without_a_credential_and_the_token_is_single_use() {
        let (app, mut rx, root) = asking().await;
        let token = mint(&app).await;
        let (status, body) = tap(&app, format!(r#"{{"token": "{token}", "choice": "Push now"}}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["answered"], "Push now");
        let command = rx.try_recv().unwrap();
        assert_eq!(command["type"], "answer");
        assert_eq!(command["question_id"], "q1");
        assert_eq!(
            command["answers"],
            json!({"Push now?": "Push now"}),
            "the cockpit's own shape"
        );
        // Spent: the same token again is a 401, with nothing further down the link.
        let (status, body) = tap(&app, format!(r#"{{"token": "{token}", "choice": "Wait"}}"#)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert!(rx.try_recv().is_err(), "one answer, once");
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn the_token_decides_what_it_answers_and_for_how_long() {
        let (app, mut rx, root) = asking().await;
        // The cockpit's own API token is not an answer token: it is just an unknown one here.
        for token in [app.api_token.clone(), crate::util::random_token()] {
            let (status, _) = tap(&app, format!(r#"{{"token": "{token}", "choice": "Push now"}}"#)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "not a minted token");
        }
        // A token whose question no longer is the open one: a conflict, and the colony keeps the
        // answer it is actually being asked for.
        let token = mint(&app).await;
        let rt = app.runtime("abc").await;
        *rt.open_question.lock().await = Some((
            "q2".into(),
            vec![json!({"question": "And now?", "options": [{"label": "Push now"}]})],
            crate::protocol::QuestionRisk::ReadOnly,
        ));
        let (status, body) = tap(&app, format!(r#"{{"token": "{token}", "choice": "Push now"}}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(rx.try_recv().is_err(), "nothing was forwarded");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Past its TTL a token is dead even while its question is still open and unanswered — and a
    /// dead token is dropped, not kept around to be tried again.
    #[tokio::test]
    async fn an_expired_token_answers_nothing_even_for_its_own_open_question() {
        let (app, mut rx, root) = asking().await;
        let token = mint(&app).await;
        app.answer_tokens
            .entries
            .lock()
            .await
            .values_mut()
            .for_each(|e| e.expires_at = Instant::now() - Duration::from_secs(1));
        let (status, body) = tap(&app, format!(r#"{{"token": "{token}", "choice": "Push now"}}"#)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert!(rx.try_recv().is_err(), "nothing was forwarded");
        assert!(app.answer_tokens.entries.lock().await.is_empty(), "the dead token is gone");
        assert!(
            app.runtime("abc").await.open_question().await.is_some(),
            "the question still stands"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A stale token is burned: once its question moved on, putting that question back (a
    /// runner that starts its `q-N` counter over after a restore) does not revive it — nor does a
    /// new question that reuses the id with different content ever accept it.
    #[tokio::test]
    async fn a_reused_question_id_does_not_revive_or_accept_an_old_token() {
        let (app, mut rx, root) = asking().await;
        let original = app.runtime("abc").await.open_question().await.unwrap();
        // Same id, other content: refused and burned.
        let token = mint(&app).await;
        let rt = app.runtime("abc").await;
        *rt.open_question.lock().await = Some((
            "q1".into(),
            vec![json!({"question": "Delete the branch?", "options": [{"label": "Push now"}, {"label": "Wait"}]})],
            crate::protocol::QuestionRisk::ReadOnly,
        ));
        let (status, body) = tap(&app, format!(r#"{{"token": "{token}", "choice": "Push now"}}"#)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        // The original question back again: the burned token stays dead.
        *rt.open_question.lock().await = Some(original);
        let (status, _) = tap(&app, format!(r#"{{"token": "{token}", "choice": "Push now"}}"#)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "burned by the conflict");
        assert!(rx.try_recv().is_err(), "nothing was forwarded");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A token is scoped to its colony: the body names no colony, so a token minted for `abc`
    /// answers `abc` only, even when another colony is asking a question with the same id and
    /// labels — and that colony's own token never lands on `abc`.
    #[tokio::test]
    async fn a_token_answers_its_own_colony_and_no_other() {
        let (app, mut rx, root) = asking().await;
        let mut other = crate::sessions::tests::colony("acme", crate::sessions::SessionStatus::WaitingForAnswer);
        other.id = "xyz".into();
        app.sessions.write().await.push(other);
        let rt_xyz = app.runtime("xyz").await;
        *rt_xyz.open_question.lock().await = app.runtime("abc").await.open_question().await;
        let mut rx_xyz = rt_xyz.commands_rx.lock().await.take().unwrap();
        let for_abc = mint(&app).await;
        let for_xyz = app.answer_tokens.for_push(&app, "xyz").await.unwrap().0;
        assert_ne!(for_abc, for_xyz);
        let (status, _) = tap(&app, format!(r#"{{"token": "{for_xyz}", "choice": "Wait"}}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(rx.try_recv().is_err(), "abc took nothing");
        assert_eq!(rx_xyz.try_recv().unwrap()["answers"], json!({"Push now?": "Wait"}));
        let (status, _) = tap(&app, format!(r#"{{"token": "{for_abc}", "choice": "Push now"}}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(rx.try_recv().unwrap()["answers"], json!({"Push now?": "Push now"}));
        assert!(rx_xyz.try_recv().is_err(), "xyz took one answer only");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The runner's own events retire a colony's tokens: `question_answered` (answered anywhere)
    /// and a new `question`. A later push mints afresh, and replaces rather than adds.
    #[tokio::test]
    async fn the_questions_own_events_revoke_its_tokens() {
        let (app, mut rx, root) = asking().await;
        let rt = app.runtime("abc").await;
        let first = mint(&app).await;
        let second = mint(&app).await;
        assert_eq!(
            app.answer_tokens.entries.lock().await.len(),
            1,
            "a push replaces the last token"
        );
        let (status, _) = tap(&app, format!(r#"{{"token": "{first}", "choice": "Wait"}}"#)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "replaced by the second push");
        let asked = r#"{"seq":1,"type":"question","question_id":"q1","questions":[{"question":"Push now?","header":"File","options":[{"label":"Push now"},{"label":"Wait"}]}],"risk":"read_only"}"#;
        crate::events::handle_agent_event(&app, "abc", &rt, asked).await;
        let (status, _) = tap(&app, format!(r#"{{"token": "{second}", "choice": "Wait"}}"#)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a new question retires it, even the same one"
        );
        let third = mint(&app).await;
        let answered = r#"{"seq":2,"type":"question_answered","question_id":"q1","answers":{}}"#;
        crate::events::handle_agent_event(&app, "abc", &rt, answered).await;
        let (status, _) = tap(&app, format!(r#"{{"token": "{third}", "choice": "Wait"}}"#)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "answered elsewhere");
        assert!(rx.try_recv().is_err(), "nothing was forwarded");
        let _ = std::fs::remove_dir_all(root);
    }

    /// An answer token is no API credential: as a Bearer or a cookie it opens nothing, and the
    /// registry never holds it in plaintext.
    #[tokio::test]
    async fn an_answer_token_is_not_an_api_credential() {
        let (app, mut rx, root) = asking().await;
        let token = mint(&app).await;
        let cockpit_answer = r#"{"question_id": "q1", "answers": {"Push now?": "Push now"}}"#;
        let mut bearer = post("/api/sessions/abc/answer", cockpit_answer);
        bearer
            .headers_mut()
            .insert(header::AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        let mut cookie = post("/api/sessions/abc/answer", cockpit_answer);
        cookie
            .headers_mut()
            .insert(header::COOKIE, format!("colonizer_token={token}").parse().unwrap());
        for req in [bearer, cookie] {
            let res = router(&app).oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }
        assert!(rx.try_recv().is_err(), "nothing was forwarded");
        let entries = app.answer_tokens.entries.lock().await;
        assert!(!entries.contains_key(&token), "keyed by hash, never the token");
        assert!(entries.contains_key(&hash_token(&token)), "and still live");
        drop(entries);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_bad_body_is_refused_without_spending_the_token() {
        let (app, mut rx, root) = asking().await;
        let token = mint(&app).await;
        let answer = format!(r#"{{"token": "{token}", "choice": "Push now"}}"#);
        for body in [
            r#"{"choice": "Push now"}"#.to_string(),                                        // no token
            r#"{"token": "", "choice": "Push now"}"#.to_string(),                           // empty token
            format!(r#"{{"token": "{token}"}}"#),                                           // no answer
            format!(r#"{{"token": "{token}", "choice": "Push now", "other": "go"}}"#),      // both
            format!(r#"{{"token": "{token}", "choice": "  "}}"#),                           // blank choice
            format!(r#"{{"token": "{token}", "other": "{}"}}"#, "x".repeat(MAX_OTHER + 1)), // too long
            format!(r#"{{"token": "{token}", "choice": "Launch it"}}"#),                    // unknown label
        ] {
            let (status, _) = tap(&app, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        }
        assert!(rx.try_recv().is_err(), "nothing was forwarded");
        // The refusals above burned nothing: the token still answers.
        let (status, _) = tap(&app, answer).await;
        assert_eq!(status, StatusCode::OK);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Free text ("Other") is the value in the answers map, under the question's text — what the
    /// cockpit's own Other field sends — and the route answers with it verbatim.
    #[tokio::test]
    async fn free_text_answers_like_the_cockpits_other_field() {
        let (app, mut rx, root) = asking().await;
        let token = mint(&app).await;
        let (status, body) = tap(&app, format!(r#"{{"token": "{token}", "other": "  push at midnight  "}}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["answered"], "push at midnight", "trimmed, verbatim");
        let command = rx.try_recv().unwrap();
        assert_eq!(command["answers"], json!({"Push now?": "push at midnight"}));
        assert_eq!(command["response"], Value::Null);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The contract race: a tap and the cockpit's own answer button at once, on separate threads
    /// and many times over. The open question's gate takes exactly one of them, so the runner gets
    /// one answer and the loser reads a 409.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_push_tap_and_a_cockpit_answer_cannot_both_go_through() {
        for _ in 0..25 {
            let (app, mut rx, root) = asking().await;
            let token = mint(&app).await;
            let tap = post(
                "/api/push/answer",
                &format!(r#"{{"token": "{token}", "choice": "Push now"}}"#),
            );
            let mut cockpit = post(
                "/api/sessions/abc/answer",
                r#"{"question_id": "q1", "answers": {"Push now?": "Wait"}}"#,
            );
            cockpit
                .headers_mut()
                .insert(header::AUTHORIZATION, format!("Bearer {}", app.api_token).parse().unwrap());
            let push = tokio::spawn(router(&app).oneshot(tap));
            let cockpit = tokio::spawn(router(&app).oneshot(cockpit));
            let (push, cockpit) = (
                push.await.unwrap().unwrap().status(),
                cockpit.await.unwrap().unwrap().status(),
            );
            assert!(
                (push == StatusCode::OK && cockpit == StatusCode::CONFLICT)
                    || (push == StatusCode::CONFLICT && cockpit == StatusCode::NO_CONTENT),
                "exactly one answer lands, the other reads 409: push {push}, cockpit {cockpit}"
            );
            assert_eq!(rx.try_recv().unwrap()["type"], "answer", "the winner reached the runner");
            assert!(rx.try_recv().is_err(), "and only one did");
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// A suspended colony has no runner to hand a tap to, so the answer is held on the record for
    /// the restore (issue #562), like the cockpit's own answer would be.
    #[tokio::test]
    async fn a_suspended_colonys_tap_is_held_not_lost() {
        let (app, mut rx, root) = asking().await;
        app.update_session("abc", |x| {
            x.suspended = Some(crate::sessions::Suspension {
                at: chrono::Utc::now(),
                snapshot: None,
                reason: crate::sessions::WAITING_FOR_ANSWER.into(),
                path: crate::sessions::SESSION_RESUME.into(),
            });
        })
        .await
        .unwrap();
        let token = mint(&app).await;
        let (status, body) = tap(&app, format!(r#"{{"token": "{token}", "choice": "Push now"}}"#)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["answered"], "Push now");
        assert!(rx.try_recv().is_err(), "no runner to receive it");
        let held = app
            .session("abc")
            .await
            .unwrap()
            .pending_answer
            .expect("held for the restore");
        assert_eq!(held.question_id, "q1");
        assert!(held.prompt.contains("Q: Push now?\nA: Push now"), "{}", held.prompt);
        let _ = std::fs::remove_dir_all(root);
    }

    /// `for_push` answers with the option labels and nothing else — no question text, no header,
    /// ever — and only for a question a notification can actually show.
    #[tokio::test]
    async fn for_push_mints_labels_only_for_answerable_questions() {
        let (app, _rx, root) = asking().await;
        let (token, labels) = app.answer_tokens.for_push(&app, "abc").await.unwrap();
        assert_eq!(labels, vec!["Push now", "Wait"], "labels, in order");
        assert_eq!(token.len(), 64, "the token is hex, hashed before it is stored");
        let payload = crate::push::question_payload(
            "acme/webshop #1 needs an answer",
            "abc",
            Some((&token, &labels)),
            &crate::push_prefs::Prefs::default(),
            None,
        );
        let body = serde_json::to_string(&payload).unwrap();
        for sentinel in ["SENTINEL-question", "SENTINEL-header", "Push now?"] {
            assert!(!body.contains(sentinel), "the push payload leaked {sentinel}: {body}");
        }
        // Only a notification-shown question mints: a multi-select, a fourth label, a second
        // question and no question at all each mint nothing.
        async fn asked(app: &Shared, questions: Vec<Value>) -> Option<(String, Vec<String>)> {
            let rt = app.runtime("abc").await;
            *rt.open_question.lock().await = Some(("q1".into(), questions, crate::protocol::QuestionRisk::ReadOnly));
            app.answer_tokens.for_push(app, "abc").await
        }
        assert!(
            asked(
                &app,
                vec![json!({"question": "q", "multi_select": true, "options": [{"label": "a"}]})]
            )
            .await
            .is_none()
        );
        assert!(
            asked(
                &app,
                vec![json!({"question": "q", "options": [
                    {"label": "a"}, {"label": "b"}, {"label": "c"}, {"label": "d"}
                ]})]
            )
            .await
            .is_none()
        );
        assert!(
            asked(
                &app,
                vec![
                    json!({"question": "q", "options": [{"label": "a"}]}),
                    json!({"question": "r", "options": []}),
                ]
            )
            .await
            .is_none()
        );
        assert!(asked(&app, vec![]).await.is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
