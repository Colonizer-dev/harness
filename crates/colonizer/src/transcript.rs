//! A colony's native session transcript (issue #736), read back over
//! `/api/sessions/{id}/transcript` and normalized to one shape.
//!
//! The agent module keeps its own conversation log; that directory is
//! host-mounted over the module's `session_resume.dir` (`boot.rs`) as
//! `<session dir>/transcripts`, so the host sees the module's own layout —
//! Claude Code's `.jsonl` under `~/.claude/projects`, Codex's rollouts under
//! `~/.codex/sessions`, a Grok session directory, an OpenCode `opencode.db` or
//! a Hermes `state.db`. `txcript` reads one `Store` per harness and folds it to
//! the canonical `Transcript<Common>`, so every module answers the same shape.
//!
//! The mount is colony-writable and `txcript` follows symlinks, so its paths
//! are never handed over: [`stage`] copies just the files a harness needs,
//! symlinks skipped, into a host-private directory removed on drop, and the
//! store reads that copy. Loading is blocking file and SQLite I/O, so it runs
//! in [`tokio::task::spawn_blocking`].

use crate::github::{RegularFileRead, read_regular_file_bytes};
use crate::{Shared, client_error};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse as _, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path as FsPath, PathBuf};
use txcript::{
    Codec, Common, Store, Transcript,
    harness::{claude_code::ClaudeStore, codex::CodexStore, grok::GrokStore, hermes::HermesStore, opencode::OpenCodeStore},
};

/// The page size `GET /api/sessions/{id}/transcript` reads when the query names
/// no `limit`, and the most one page carries whatever `limit` asks.
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 500;

/// The caps the [`stage`] walk and copy obey, as a real request uses them: how
/// deep it descends, the most entries it visits, and the most bytes one file
/// and all files in sum may be.
const SNAPSHOT_DEPTH: usize = 8;
const SNAPSHOT_ENTRIES: usize = 10_000;
const SNAPSHOT_FILE_BYTES: u64 = 64 << 20;
const SNAPSHOT_TOTAL_BYTES: u64 = 256 << 20;

/// The bounds [`stage`] runs under; a test passes tiny ones.
#[derive(Clone, Copy)]
struct Caps {
    depth: usize,
    entries: usize,
    file_bytes: u64,
    total_bytes: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            depth: SNAPSHOT_DEPTH,
            entries: SNAPSHOT_ENTRIES,
            file_bytes: SNAPSHOT_FILE_BYTES,
            total_bytes: SNAPSHOT_TOTAL_BYTES,
        }
    }
}

/// The query `GET /api/sessions/{id}/transcript` takes: the format (only
/// `common` is served today, leaving room for others) and the paging pair
/// `GET /api/sessions` uses. Both stay strings so a malformed value is this
/// route's own **400**, not the extractor's plain-text rejection.
#[derive(Deserialize, Default)]
pub(crate) struct TranscriptQuery {
    format: Option<String>,
    limit: Option<String>,
    cursor: Option<String>,
}

/// The `txcript` harness a colony's transcript is read as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Harness {
    ClaudeCode,
    Codex,
    OpenCode,
    Grok,
    Hermes,
}

impl Harness {
    /// The harness an agent module id maps to, or [`TranscriptError::Unsupported`]
    /// for one with no reader (`acp`, `pi`, anything unknown).
    fn of(agent: &str) -> Result<Harness, TranscriptError> {
        Ok(match agent {
            "claude-code" => Harness::ClaudeCode,
            "codex" => Harness::Codex,
            "opencode" => Harness::OpenCode,
            "grok-build" => Harness::Grok,
            "hermes" => Harness::Hermes,
            other => {
                return Err(TranscriptError::Unsupported {
                    agent: other.to_string(),
                });
            }
        })
    }

    /// The id the answer names, which is `txcript`'s spelling of the harness.
    fn id(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude_code",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
            Harness::Grok => "grok",
            Harness::Hermes => "hermes",
        }
    }

    /// Whether a file at `rel` (relative to the mount root) is one this harness
    /// needs copied — the same set `txcript` looks for, so the copy carries the
    /// sessions and nothing else.
    fn wants(self, rel: &FsPath) -> bool {
        let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
        match self {
            Harness::ClaudeCode => name.ends_with(".jsonl"),
            Harness::Codex => (rel.starts_with("sessions") || rel.starts_with("archived_sessions")) && name.ends_with(".jsonl"),
            Harness::Grok => rel.starts_with("sessions"),
            Harness::OpenCode => matches!(name, "opencode.db" | "opencode.db-wal" | "opencode.db-shm"),
            Harness::Hermes => name.starts_with("state.db"),
        }
    }
}

/// Why a colony's transcript could not be read. [`refused`] is the one place
/// these map to HTTP statuses.
#[derive(Debug)]
enum TranscriptError {
    /// The agent module has no `txcript` harness to read its transcripts as.
    Unsupported { agent: String },
    /// No session was recorded — a module that does not persist one, or one
    /// that has not written its store yet. An unreadable store reads this way
    /// too, since `txcript` discovers with `.ok()`.
    NotRecorded,
    /// The native store is larger than the caps allow.
    TooLarge,
    /// A native file or database is there but does not parse.
    Load(txcript::Error),
    /// The host could not stage the copy it hands to `txcript` — our side, not
    /// the guest's.
    Staged(std::io::Error),
}

/// The HTTP answer for a transcript that could not be read — the single source
/// of the mapping: an unsupported module is **422**, nothing recorded is the
/// **404** an unknown colony gets, an over-cap store is a **413**, and a store
/// that will not load or stage is a **500** whose body is deliberately generic.
fn refused(error: TranscriptError) -> Response {
    match error {
        TranscriptError::Unsupported { agent } => client_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("the \"{agent}\" agent keeps no session transcript this harness can read"),
        )
        .into_response(),
        TranscriptError::NotRecorded => {
            client_error(StatusCode::NOT_FOUND, "this colony has no session transcript recorded").into_response()
        }
        TranscriptError::TooLarge => client_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "this colony's session transcript is too large to read",
        )
        .into_response(),
        TranscriptError::Load(error) => unreadable(&error),
        TranscriptError::Staged(error) => unreadable(&error),
    }
}

/// The generic **500** for a store that would not load or stage: the detail is
/// logged, never returned, so no host path leaks.
fn unreadable(error: &dyn std::fmt::Display) -> Response {
    eprintln!("transcript: {error}");
    client_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "could not read the colony's session transcript",
    )
    .into_response()
}

/// The **400** a malformed paging or format query answers.
fn wrong_input(message: impl std::fmt::Display) -> Response {
    client_error(StatusCode::BAD_REQUEST, &message.to_string()).into_response()
}

/// A host-private staging directory, `0700` under the system temp dir and
/// removed when it drops, so nothing the guest can reach holds the copy
/// `txcript` is pointed at. The name carries a full UUID so the `mkdir` is
/// effectively uncontended, and it is a single `mkdir(0700)` — not a recursive
/// create — so a pre-existing directory, or a symlink to one, at that path
/// fails with `AlreadyExists` instead of being adopted.
struct Staging {
    dir: PathBuf,
}

impl Staging {
    fn new() -> Result<Staging, TranscriptError> {
        let dir = std::env::temp_dir().join(format!("colonizer-transcript-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(TranscriptError::Staged)?;
        Ok(Staging { dir })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Copy the sessions `harness` needs out of the colony-writable `transcripts`
/// mount into the host-private `dest`, so `txcript` never opens a guest path.
/// The walk skips every symlink — never followed, never descended — so neither
/// `evil.jsonl -> /etc/shadow` nor `sessions -> /` is copied; each file is then
/// read through one [`read_regular_file_bytes`] handle (`O_NOFOLLOW`, fstat,
/// `take`), so a final component swapped for a symlink fails the open rather
/// than resolving, and it must also canonicalize under the mount root (the
/// `maps.rs` pattern). Only that final component's race is closed — a directory
/// component swapped mid-walk would need an `openat` walk, and this crate
/// depends on neither `rustix` nor `nix`.
fn stage(harness: Harness, transcripts: &FsPath, dest: &FsPath, caps: Caps) -> Result<(), TranscriptError> {
    let root = std::fs::canonicalize(transcripts).ok();
    let mut seen = 0usize;
    let mut total = 0u64;
    // (directory, its path relative to the mount, depth below it)
    let mut queue = vec![(transcripts.to_path_buf(), PathBuf::new(), 0usize)];
    while let Some((dir, rel, depth)) = queue.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            seen += 1;
            if seen > caps.entries {
                return Err(TranscriptError::TooLarge);
            }
            // `file_type` is the lstat answer: a symlink reports itself.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let child = rel.join(entry.file_name());
            if kind.is_dir() {
                if depth + 1 > caps.depth {
                    return Err(TranscriptError::TooLarge);
                }
                queue.push((entry.path(), child, depth + 1));
            } else if kind.is_file() && harness.wants(&child) {
                let src = entry.path();
                let inside = matches!((&root, std::fs::canonicalize(&src).ok()),
                    (Some(root), Some(real)) if real.starts_with(root));
                if !inside {
                    continue;
                }
                let bytes = match read_regular_file_bytes(&src, caps.file_bytes) {
                    Ok(bytes) => bytes,
                    // A symlink, a FIFO or a vanished file is not a session.
                    Err(RegularFileRead::Unreadable(_)) => continue,
                    Err(RegularFileRead::TooLarge(_)) => return Err(TranscriptError::TooLarge),
                };
                total += bytes.len() as u64;
                if total > caps.total_bytes {
                    return Err(TranscriptError::TooLarge);
                }
                let out = dest.join(&child);
                if let Some(parent) = out.parent() {
                    std::fs::create_dir_all(parent).map_err(TranscriptError::Staged)?;
                }
                let mut file = std::fs::File::create(&out).map_err(TranscriptError::Staged)?;
                file.write_all(&bytes).map_err(TranscriptError::Staged)?;
                // Keep the native mtime so `newest`'s tie-break still means something.
                if let Ok(mtime) = std::fs::metadata(&src).and_then(|meta| meta.modified()) {
                    let _ = file.set_times(std::fs::FileTimes::new().set_modified(mtime));
                }
            }
        }
    }
    Ok(())
}

/// The modification time of a file-backed session, the tie-break [`newest`]
/// uses when two sessions carry the same recorded timestamp. Read off the
/// snapshot copy, whose mtime [`stage`] preserves.
fn file_mtime(path: &PathBuf) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|meta| meta.modified().ok())
}

/// The most recent session a store holds, loaded and folded to [`Common`]: the
/// one with the newest recorded timestamp, a file's mtime breaking a tie.
/// `None` when the store is empty.
fn newest<S>(
    store: &S,
    fallback: impl Fn(&S::Ref) -> Option<std::time::SystemTime>,
) -> Result<Option<Transcript<Common>>, TranscriptError>
where
    S: Store,
    S::H: Codec,
{
    let discovered = store.discover().map_err(TranscriptError::Load)?;
    let Some(best) = discovered
        .into_iter()
        .max_by_key(|found| (found.meta.timestamp, fallback(&found.reference)))
    else {
        return Ok(None);
    };
    let native = store.load(&best.reference).map_err(TranscriptError::Load)?;
    Ok(Some(<S::H as Codec>::to_common(&native).map_err(TranscriptError::Load)?))
}

/// Read the most recent native session under `<session dir>/transcripts` for
/// one harness: the canonical transcript, [`TranscriptError::NotRecorded`]
/// when the module kept none.
fn read(harness: Harness, transcripts: &FsPath, caps: Caps) -> Result<Transcript<Common>, TranscriptError> {
    // The mount is guest-writable and `txcript` follows symlinks, so stage a
    // symlink-free copy and point the store at that instead.
    let staging = Staging::new()?;
    stage(harness, transcripts, &staging.dir, caps)?;
    let root = staging.dir.as_path();
    let found = match harness {
        Harness::ClaudeCode => newest(&ClaudeStore::new(root), file_mtime)?,
        Harness::Codex => newest(&CodexStore::new(root.join("sessions")), file_mtime)?,
        Harness::Grok => newest(&GrokStore::new(root.join("sessions")), file_mtime)?,
        // The SQLite harnesses keep one database at the root of their data dir;
        // their rows carry the timestamp, so no file mtime is consulted.
        Harness::OpenCode => newest(&OpenCodeStore::new(root.join("opencode.db")), |_: &String| None)?,
        Harness::Hermes => newest(&HermesStore::new(root.join("state.db")), |_: &String| None)?,
    };
    found.ok_or(TranscriptError::NotRecorded)
}

/// `GET /api/sessions/{id}/transcript?format=common&limit=&cursor=`: the
/// colony's own agent transcript, normalized to the canonical message shape
/// and paged. The same visibility guard as the diff and files routes applies,
/// so an out-of-scope token reads an unknown colony; a read that fails is
/// answered by [`refused`].
pub(crate) async fn transcript(
    State(app): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<TranscriptQuery>,
    scoped: Option<axum::Extension<crate::api_tokens::ScopedToken>>,
) -> Response {
    let Some(session) = crate::sessions::visible_session(&app, &id, scoped.as_ref()).await else {
        return client_error(StatusCode::NOT_FOUND, "no such session").into_response();
    };
    // `format` is required and only `common` is served; the parameter exists so
    // a later format can be added without changing the route's shape.
    if query.format.as_deref() != Some("common") {
        return wrong_input("`format` is required and must be `common`");
    }
    let harness = match Harness::of(&session.agent) {
        Ok(harness) => harness,
        Err(error) => return refused(error),
    };
    let dir = app.session_dir(&id).join("transcripts");
    // Blocking file and SQLite I/O: keep it off the async runtime's threads.
    let loaded = match tokio::task::spawn_blocking(move || read(harness, &dir, Caps::default())).await {
        Ok(loaded) => loaded,
        // A panic payload can carry host paths or guest content, so it is logged
        // and answered with the same generic body as any other read failure.
        Err(join) => return unreadable(&join),
    };
    let transcript = match loaded {
        Ok(transcript) => transcript,
        Err(error) => return refused(error),
    };

    let Transcript { meta, body: messages } = transcript;
    let total = messages.len();
    let limit = match query.limit.as_deref() {
        None => DEFAULT_LIMIT,
        Some(raw) => match raw.parse::<usize>() {
            Ok(parsed) => parsed.clamp(1, MAX_LIMIT),
            Err(_) => return wrong_input(format!("`limit` must be an integer between 1 and {MAX_LIMIT}")),
        },
    };
    // The cursor is the index of the last message already delivered, so the page
    // starts right after it; one naming no message is refused, the way the
    // colony list refuses a cursor naming no colony.
    let start = match query.cursor.as_deref() {
        None => 0,
        Some(raw) => match raw.parse::<usize>() {
            Ok(index) if index < total => index + 1,
            _ => return wrong_input("`cursor` names no message in this transcript; read a page and send back its `next_cursor`"),
        },
    };
    let end = (start + limit).min(total);
    let next_cursor = (end < total).then(|| (end - 1).to_string());
    let page: Vec<&txcript::common::Message> = messages.iter().skip(start).take(end - start).collect();

    Json(json!({
        "agent": session.agent,
        "harness": harness.id(),
        "meta": meta,
        "messages": page,
        "total": total,
        "next_cursor": next_cursor,
    }))
    .into_response()
}

/// The API routes this module serves. `server::api_routes` merges them into
/// the cockpit's router, behind the activity log's route layer and
/// `host_guard`.
pub(crate) fn routes() -> axum::Router<Shared> {
    use axum::routing::get;
    axum::Router::new().route("/api/sessions/{id}/transcript", get(transcript))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStatus;
    use crate::sessions::tests::app_with_colony;
    use serde_json::Value;
    use txcript::common::{Block, Message, Role};

    /// The hand-written fixtures, embedded so the tests do not depend on the
    /// working directory.
    const CLAUDE_FIXTURE: &str = include_str!("../tests/fixtures/transcripts/claude_code.jsonl");
    const CODEX_FIXTURE: &str = include_str!("../tests/fixtures/transcripts/codex.jsonl");
    const GROK_FIXTURE: &str = include_str!("../tests/fixtures/transcripts/grok_chat_history.jsonl");
    const OPENCODE_SQL: &str = include_str!("../tests/fixtures/transcripts/opencode.sql");

    /// A stopped colony whose `transcripts/` holds what `plant` writes.
    async fn colony_with(id: &str, agent: &str, plant: impl FnOnce(&FsPath)) -> (Shared, PathBuf) {
        let (app, root) = app_with_colony(id, SessionStatus::Stopped).await;
        app.update_session(id, |session| session.agent = agent.to_string()).await;
        let transcripts = app.session_dir(id).join("transcripts");
        std::fs::create_dir_all(&transcripts).unwrap();
        plant(&transcripts);
        (app, root)
    }

    /// Read what `read` answers for a freshly planted colony, with `caps`.
    async fn read_with(
        id: &str,
        agent: &str,
        caps: Caps,
        plant: impl FnOnce(&FsPath),
    ) -> (Result<Transcript<Common>, TranscriptError>, PathBuf) {
        let (app, root) = colony_with(id, agent, plant).await;
        let dir = app.session_dir(id).join("transcripts");
        (read(Harness::of(agent).unwrap(), &dir, caps), root)
    }

    /// A unique scratch directory outside any colony, for the escape tests.
    fn outside_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("colonizer-transcript-{tag}-{}", crate::util::short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The claude-code layout: the projects root is the mount, one file per
    /// session under a slug.
    fn plant_claude(transcripts: &FsPath) {
        let dir = transcripts.join("-work-repo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sess-1.jsonl"), CLAUDE_FIXTURE).unwrap();
    }

    /// The codex layout: rollouts under `sessions/<yyyy>/<mm>/<dd>/`.
    fn plant_codex(transcripts: &FsPath) {
        let dir = transcripts.join("sessions").join("2026").join("04").join("01");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rollout-2026-04-01T00-00-00-sess-1.jsonl"), CODEX_FIXTURE).unwrap();
    }

    /// The grok layout: a session directory holding `chat_history.jsonl`.
    fn plant_grok(transcripts: &FsPath) {
        let dir = transcripts.join("sessions").join("%2Frepo").join("sess-grok");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("chat_history.jsonl"), GROK_FIXTURE).unwrap();
    }

    /// The opencode layout: one SQLite database at the mount root.
    fn plant_opencode(transcripts: &FsPath) {
        let conn = rusqlite::Connection::open(transcripts.join("opencode.db")).unwrap();
        conn.execute_batch(OPENCODE_SQL).unwrap();
    }

    /// The shared shape every native store folds to: a user turn, an assistant
    /// reply with one tool call, and its result.
    fn assert_conversation(messages: &[Message], expected_roles: &[&str]) {
        let roles = messages
            .iter()
            .map(|message| match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            })
            .collect::<Vec<_>>();
        let kinds = messages
            .iter()
            .flat_map(|message| message.content.iter())
            .map(|block| match block {
                Block::Text { .. } => "text",
                Block::Thinking { .. } => "thinking",
                Block::ToolUse { .. } => "tool_use",
                Block::ToolResult { .. } => "tool_result",
                Block::Image { .. } => "image",
                Block::Artifact { .. } => "artifact",
            })
            .collect::<Vec<_>>();
        assert_eq!(roles.as_slice(), expected_roles, "{messages:#?}");
        assert!(kinds.contains(&"tool_use"), "no tool_use in {messages:#?}");
        assert!(kinds.contains(&"tool_result"), "no tool_result in {messages:#?}");
    }

    /// Four hand-written native stores all fold to the same canonical shape.
    #[tokio::test]
    async fn each_supported_module_loads_its_native_transcript() {
        type Plant = fn(&FsPath);
        let cases: [(&str, &str, Plant, &[&str]); 4] = [
            (
                "claude",
                "claude-code",
                plant_claude,
                &["user", "assistant", "user", "assistant"],
            ),
            ("codex", "codex", plant_codex, &["user", "assistant", "user", "assistant"]),
            ("grok", "grok-build", plant_grok, &["user", "assistant", "user", "assistant"]),
            (
                "opencode",
                "opencode",
                plant_opencode,
                &["user", "assistant", "assistant", "user", "assistant"],
            ),
        ];
        for (id, agent, plant, expected_roles) in cases {
            let (result, root) = read_with(id, agent, Caps::default(), plant).await;
            assert_conversation(&result.expect("the fixture loads").body, expected_roles);
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// The harness id each module folds to, and the ones with no reader.
    #[test]
    fn agent_modules_map_to_harnesses() {
        for (agent, id) in [
            ("claude-code", "claude_code"),
            ("codex", "codex"),
            ("opencode", "opencode"),
            ("grok-build", "grok"),
            ("hermes", "hermes"),
        ] {
            assert_eq!(Harness::of(agent).unwrap().id(), id);
        }
        for agent in ["acp", "pi", ""] {
            assert!(
                matches!(Harness::of(agent), Err(TranscriptError::Unsupported { .. })),
                "{agent:?} has no transcript reader"
            );
        }
    }

    /// A symlinked file, and a symlinked directory, are never followed: a
    /// planted link cannot read outside the colony's own transcripts.
    #[tokio::test]
    async fn a_symlink_is_never_followed() {
        let outside = outside_dir("link");
        let secret = outside.join("evil.jsonl");
        std::fs::write(&secret, CLAUDE_FIXTURE).unwrap();
        let (result, root) = read_with("abc", "claude-code", Caps::default(), |transcripts| {
            let slug = transcripts.join("-work-repo");
            std::fs::create_dir_all(&slug).unwrap();
            std::os::unix::fs::symlink(&secret, slug.join("evil.jsonl")).unwrap();
        })
        .await;
        assert!(matches!(result, Err(TranscriptError::NotRecorded)), "{result:?}");
        let _ = std::fs::remove_dir_all(root);

        // A `sessions` directory symlinked to a real one outside is not followed.
        let day = outside.join("2026").join("04").join("01");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("rollout-2026-04-01T00-00-00-sess-1.jsonl"), CODEX_FIXTURE).unwrap();
        let (result, root) = read_with("abc", "codex", Caps::default(), |transcripts| {
            std::os::unix::fs::symlink(&outside, transcripts.join("sessions")).unwrap();
        })
        .await;
        assert!(matches!(result, Err(TranscriptError::NotRecorded)), "{result:?}");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    /// A file over the per-file cap is [`TranscriptError::TooLarge`], which the
    /// route answers as a **413**.
    #[tokio::test]
    async fn an_oversized_file_is_refused() {
        let tiny = Caps {
            file_bytes: 8,
            ..Caps::default()
        };
        let (result, root) = read_with("abc", "claude-code", tiny, plant_claude).await;
        assert!(matches!(result, Err(TranscriptError::TooLarge)), "{result:?}");
        assert_eq!(refused(TranscriptError::TooLarge).status(), StatusCode::PAYLOAD_TOO_LARGE);
        let _ = std::fs::remove_dir_all(root);
    }

    fn query(format: Option<&str>, limit: Option<&str>, cursor: Option<&str>) -> TranscriptQuery {
        TranscriptQuery {
            format: format.map(String::from),
            limit: limit.map(String::from),
            cursor: cursor.map(String::from),
        }
    }

    async fn get(app: &Shared, id: &str, query: TranscriptQuery) -> Response {
        transcript(State(app.clone()), Path(id.into()), Query(query), None).await
    }

    async fn body(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The route answers the canonical transcript and pages over its messages
    /// with the colony list's cursor convention, ending at `null`.
    #[tokio::test]
    async fn the_route_pages_over_messages_with_a_cursor() {
        let (app, root) = colony_with("abc", "claude-code", plant_claude).await;

        let response = get(&app, "abc", query(Some("common"), Some("2"), None)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = body(response).await;
        assert_eq!(page["agent"], "claude-code");
        assert_eq!(page["harness"], "claude_code");
        assert_eq!(page["total"], 4);
        assert_eq!(page["messages"].as_array().unwrap().len(), 2);
        assert_eq!(page["messages"][0]["role"], "user");
        assert_eq!(page["next_cursor"], "1", "the page's last index is the next cursor");

        let response = get(&app, "abc", query(Some("common"), Some("2"), Some("1"))).await;
        assert_eq!(response.status(), StatusCode::OK);
        let page = body(response).await;
        assert_eq!(page["messages"].as_array().unwrap().len(), 2, "the cursor starts after it");
        assert_eq!(page["next_cursor"], Value::Null, "nothing follows the last message");

        // A whole-list page carries everything and ends at null.
        let response = get(&app, "abc", query(Some("common"), None, None)).await;
        let page = body(response).await;
        assert_eq!(page["messages"].as_array().unwrap().len(), 4);
        assert_eq!(page["next_cursor"], Value::Null);
        let _ = std::fs::remove_dir_all(root);
    }

    /// `format` is required and must be `common`; a malformed cursor or limit
    /// is this route's own **400**.
    #[tokio::test]
    async fn the_query_is_checked_with_a_four_hundred() {
        let (app, root) = colony_with("abc", "claude-code", plant_claude).await;
        for bad in [query(None, None, None), query(Some("openai"), None, None)] {
            let response = get(&app, "abc", bad).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(body(response).await["error"], "`format` is required and must be `common`");
        }
        let response = get(&app, "abc", query(Some("common"), Some("soon"), None)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "a malformed limit is refused");
        for cursor in ["nope", "4", "99"] {
            let response = get(&app, "abc", query(Some("common"), None, Some(cursor))).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{cursor}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// An unknown colony and one outside a scoped token's limits read the same
    /// **404**; a module with no reader is **422**; a supported module with
    /// nothing recorded is **404**.
    #[tokio::test]
    async fn unsupported_and_unrecorded_colonies_read_as_errors() {
        let (app, root) = colony_with("abc", "claude-code", plant_claude).await;

        let response = get(&app, "zzz", query(Some("common"), None, None)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let token = axum::Extension(crate::api_tokens::ScopedToken {
            id: "tok_test".into(),
            name: "watcher".into(),
            scope: crate::api_tokens::Scope::Read,
            orgs: Vec::new(),
            repos: vec!["other/repo".into()],
            max_concurrent: None,
            budget_usd_per_day: None,
        });
        let response = transcript(
            State(app.clone()),
            Path("abc".into()),
            Query(query(Some("common"), None, None)),
            Some(token),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "outside the limits reads as unknown"
        );

        // acp has no txcript reader.
        app.update_session("abc", |session| session.agent = "acp".into()).await;
        let response = get(&app, "abc", query(Some("common"), None, None)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        // A module that persists nothing yet has no store under transcripts/.
        let (bare, bare_root) = app_with_colony("bare", SessionStatus::Stopped).await;
        bare.update_session("bare", |session| session.agent = "claude-code".into())
            .await;
        let response = get(&bare, "bare", query(Some("common"), None, None)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body(response).await["error"],
            "this colony has no session transcript recorded"
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(bare_root);
    }
}
