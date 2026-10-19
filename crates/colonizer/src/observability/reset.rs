//! `POST /api/observability/reset` (issue #850): forget where export got to, so the next start
//! begins at "now".
//!
//! Deleting `<data>/observability/state.json` drops the committed cursors *and* the settled
//! destination list the add-on keeps in the same file, so its next start settles the destination
//! again under **Start from**: `now` (the default) binds every existing ledger at its end and sends
//! only what is written from there; `backlog` re-reads within **Max backlog (days)**. With
//! `hash_key: true` the pseudonym key (`hash.key`) goes too — new pseudonyms are minted from the
//! new key, so series grouped by org or repo name do not line up with the old ones.
//!
//! Nothing else is touched: not the ledgers (they are the source of truth, and the backfill reads
//! them), not the file sink's output, not `status.json`, `backfill.json` or the contract — a reset
//! is a cursor reset, not an uninstall ([docs/observability/reset.md](../../../docs/observability/reset.md)).
//!
//! A running child holds its cursors in memory and would rewrite the file over the deletion, so
//! the route parks a per-data-dir *pending* entry that the supervision loop (supervisor.rs) sees on
//! its next tick: it stops the child and starts nothing until the entry goes. The entry stands for
//! the whole handshake — waiting, deleting and only then clearing — and it carries its own expiry so
//! a request that dies mid-reset cannot park export for good. The route waits for the loop's
//! acknowledgement before deleting; if the child has not stopped by [`SETTLE_BY`] it refuses (409)
//! and deletes nothing, because a child that may still be up would rewrite the file back.
//!
//! Three refusals, all 409 and none of them deleting: a backfill is registered (it reads
//! `state.json` to know where history ends, and moving that line mid-run would replay or skip
//! records); another reset already holds the pending entry for this data dir (they would share one
//! entry, and the first to answer would release the supervisor under the second); and the exporter
//! is still stopping.

use super::supervisor;
use crate::{ApiResult, Shared, client_error};
use axum::{Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// How long the route waits for the supervision loop to acknowledge a pending reset and stop the
/// child: one reconcile tick, plus the child's stop allowance, and change.
const SETTLE_BY: Duration = Duration::from_secs(17);
/// How long a pending entry binds the supervision loop. Longer than [`SETTLE_BY`'], so a live
/// request never sees its own entry expire under it; short enough that a request cancelled
/// mid-reset leaves export held back for seconds, not for good.
const PENDING_TTL: Duration = Duration::from_secs(30);
/// How often the route re-reads the supervisor's word while it waits.
const POLL: Duration = Duration::from_millis(100);

/// The resets in flight, per data dir, with when they registered — the same one-per-data-dir
/// shape as the supervisor's `runtimes()` and `backfills()`.
fn pendings() -> &'static Mutex<BTreeMap<PathBuf, Instant>> {
    static P: OnceLock<Mutex<BTreeMap<PathBuf, Instant>>> = OnceLock::new();
    P.get_or_init(Mutex::default)
}

/// Whether a reset is pending for this data dir, asked by the supervision loop each tick: stop the
/// child and start nothing while one stands. An entry older than [`PENDING_TTL`] is dropped on the
/// floor along with the rest that expired, so a cancelled request cannot hold export off forever.
pub(super) fn pending(data_dir: &Path) -> bool {
    pendings()
        .lock()
        .map(|mut map| {
            map.retain(|_, at| at.elapsed() < PENDING_TTL);
            map.contains_key(data_dir)
        })
        .unwrap_or(false)
}

/// Registers a reset in flight, or refuses one already in flight: two resets on one data dir would
/// share the single pending entry, and whichever answered first would clear it out from under the
/// other — which would then delete into a child the supervisor had already been told to stop.
/// `false` means another reset holds the entry.
fn register(data_dir: &Path) -> bool {
    let Ok(mut map) = pendings().lock() else {
        return false;
    };
    let now = Instant::now();
    map.retain(|_, at| at.elapsed() < PENDING_TTL);
    if map.contains_key(data_dir) {
        return false;
    }
    map.insert(data_dir.to_path_buf(), now);
    true
}

fn clear(data_dir: &Path) {
    if let Ok(mut map) = pendings().lock() {
        map.remove(data_dir);
    }
}

/// `<data>/observability/state.json` — the cursors, and the settled destinations in `extra`. The
/// add-on's `state.rs` spells the same path; the mothership does not link it, so the constant is
/// repeated here like the contract's.
fn state_file(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join("state.json")
}

/// `<data>/observability/hash.key` — the pseudonym key `repo_names = hashed` hashes org and repo
/// names with (metrics.rs on this side, hashing.rs on the add-on's).
fn hash_file(data_dir: &Path) -> PathBuf {
    data_dir.join("observability").join("hash.key")
}

/// The files this reset names, with their sizes for the dry run's list: `state.json` when it is
/// there, `hash.key` only when asked for. Nothing else is ever a candidate.
fn targets(data_dir: &Path, hash_key: bool) -> Vec<(PathBuf, u64)> {
    [(state_file(data_dir), true), (hash_file(data_dir), hash_key)]
        .into_iter()
        .filter(|(path, wanted)| *wanted && path.is_file())
        .filter_map(|(path, _)| std::fs::metadata(&path).ok().map(|m| (path, m.len())))
        .collect()
}

/// Waits until no exporter child can be live for this data dir. Settled means the supervision loop
/// has acknowledged the pending entry (`resetting`), or its last word names a state no live child
/// is in (`off`, `invalid`, `refused`, `no_addon`, `failed`, `restarting`), or there is no word at
/// all — no loop running, so no child it is keeping alive. `running` and `starting` keep waiting,
/// up to [`SETTLE_BY`]. Past it the answer is `false` and the route refuses (409) rather than
/// deletes: a child that may still be up holds its cursors in memory and would rewrite
/// `state.json` over the deletion, undoing the reset — the operator can ask again.
async fn wait_until_no_child(data_dir: &Path) -> bool {
    // Tokio's clock, not the wall's: the deadline is an await, and a test may run it on paused
    // time.
    let deadline = tokio::time::Instant::now() + SETTLE_BY;
    while matches!(supervisor::runtime_state(data_dir), "running" | "starting") && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }
    !matches!(supervisor::runtime_state(data_dir), "running" | "starting")
}

/// The `POST /api/observability/reset` body. Owner-only — the route is nowhere in the feature's
/// `token_scope`, so a scoped token meets `api_tokens`' owner-only default.
#[derive(Deserialize)]
pub(super) struct ResetRequest {
    /// List the files with their sizes and delete nothing.
    #[serde(default)]
    dry_run: bool,
    /// Also delete `hash.key`, minting new pseudonyms for hashed org and repo names.
    #[serde(default)]
    hash_key: bool,
}

fn listed(targets: &[(PathBuf, u64)]) -> Vec<Value> {
    targets
        .iter()
        .map(|(path, bytes)| json!({"path": path.display().to_string(), "bytes": bytes}))
        .collect()
}

/// `POST /api/observability/reset` (issue #850): delete the exporter's committed cursors so the
/// next start begins at "now", stopping the child first (it holds the cursors in memory and would
/// rewrite the file) and letting the supervision loop restart it on its next tick. `dry_run` lists
/// what would go; `hash_key` adds the pseudonym key. 409 while a backfill runs, while another
/// reset is in flight, and while the child has not stopped by [`SETTLE_BY`] — nothing is deleted
/// in any of those three.
pub(super) async fn reset(State(app): State<Shared>, Json(body): Json<ResetRequest>) -> ApiResult<Value> {
    let data_dir = app.cfg.data_dir.clone();
    if supervisor::backfill_running(&data_dir) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "a backfill is running and reads the exporter's state to know where history ends; cancel it first",
        ));
    }
    let targets = targets(&data_dir, body.hash_key);
    if body.dry_run {
        return Ok(Json(json!({"dry_run": true, "delete": listed(&targets)})));
    }
    if !register(&data_dir) {
        return Err(client_error(
            StatusCode::CONFLICT,
            "another reset is already in flight for this data directory; wait for it to finish",
        ));
    }

    let key = hash_file(&data_dir);
    let outcome = async {
        if !wait_until_no_child(&data_dir).await {
            return Err(client_error(
                StatusCode::CONFLICT,
                "the exporter is still stopping; try again",
            ));
        }
        let mut deleted = Vec::new();
        for (path, bytes) in &targets {
            match tokio::fs::remove_file(path).await {
                Ok(()) => deleted.push(json!({"path": path.display().to_string(), "bytes": bytes})),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(client_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("could not delete {}: {e}", path.display()),
                    ));
                }
            }
        }
        Ok(deleted)
    }
    .await;
    // Always: a pending entry left behind would hold the supervisor back for the TTL.
    clear(&data_dir);
    let deleted = outcome?;
    // Only once the deletion has actually happened, and only for a key that was among the files
    // that went: the scrape endpoint parks the old key, and the next hashed scrape loads what is
    // on disk now — a refusal or a missing key leaves it where it belongs.
    if deleted.iter().any(|d| d["path"] == json!(key.display().to_string())) {
        super::metrics::forget_hash_key();
    }
    Ok(Json(json!({"reset": true, "deleted": deleted})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::{Method, header};
    use tower::ServiceExt as _;

    /// Every file under the data dir, as `path→bytes`, so a before/after diff names exactly what
    /// changed and nothing that merely moved.
    fn listing(data_dir: &Path) -> Vec<(String, u64)> {
        fn walk(dir: &Path, out: &mut Vec<(String, u64)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push((path.display().to_string(), std::fs::metadata(&path).unwrap().len()));
                }
            }
        }
        let mut out = Vec::new();
        walk(data_dir, &mut out);
        out
    }

    /// The reset's blast radius, as a fixture: the state, the key, the add-on's own files, the
    /// file sink's output, the contract, and one source ledger at the data dir's root.
    fn seed(data_dir: &Path) {
        let obs = data_dir.join("observability");
        std::fs::create_dir_all(obs.join("otlp")).unwrap();
        std::fs::write(state_file(data_dir), br#"{"version":1,"cursors":[]}"#).unwrap();
        std::fs::write(hash_file(data_dir), [7u8; 32]).unwrap();
        std::fs::write(supervisor::status_path(data_dir), br#"{"state":"running"}"#).unwrap();
        std::fs::write(obs.join("backfill.json"), b"{}").unwrap();
        std::fs::write(obs.join("exporter.json"), b"{}").unwrap();
        std::fs::write(obs.join("otlp").join("logs-1.jsonl"), b"one line\n").unwrap();
        std::fs::write(data_dir.join("activity.jsonl"), b"{}").unwrap();
    }

    fn request(dry_run: bool, hash_key: bool) -> Json<ResetRequest> {
        Json(ResetRequest { dry_run, hash_key })
    }

    #[tokio::test]
    async fn a_reset_deletes_only_state_json() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        let before = listing(&app.cfg.data_dir);
        let Json(answer) = reset(State(app.clone()), request(false, false)).await.unwrap();
        assert_eq!(answer["reset"], json!(true), "{answer}");
        assert_eq!(
            answer["deleted"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|d| d["path"].as_str())
                .collect::<Vec<_>>(),
            vec![state_file(&app.cfg.data_dir).display().to_string()],
            "the answer names state.json and nothing else: {answer}"
        );
        let after = listing(&app.cfg.data_dir);
        let gone: Vec<_> = before.iter().filter(|f| !after.contains(f)).cloned().collect();
        assert_eq!(gone.len(), 1, "exactly one file went: {gone:?}");
        assert_eq!(gone[0].0, state_file(&app.cfg.data_dir).display().to_string());
        assert!(hash_file(&app.cfg.data_dir).is_file(), "the hash key stays unless asked for");
        assert!(
            app.cfg.data_dir.join("activity.jsonl").is_file(),
            "the source ledgers are never touched"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn hash_key_true_also_deletes_the_key() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        let Json(answer) = reset(State(app.clone()), request(false, true)).await.unwrap();
        let deleted: Vec<_> = answer["deleted"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d["path"].as_str())
            .collect();
        assert!(
            deleted.contains(&hash_file(&app.cfg.data_dir).display().to_string().as_str())
                && deleted.contains(&state_file(&app.cfg.data_dir).display().to_string().as_str())
                && deleted.len() == 2,
            "both named files went, and nothing else: {answer}"
        );
        assert!(!hash_file(&app.cfg.data_dir).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_dry_run_deletes_nothing_and_lists_what_would_go() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        let before = listing(&app.cfg.data_dir);
        let Json(answer) = reset(State(app.clone()), request(true, false)).await.unwrap();
        assert_eq!(answer["dry_run"], json!(true), "{answer}");
        let state_line = &answer["delete"][0];
        assert_eq!(state_line["path"], json!(state_file(&app.cfg.data_dir).display().to_string()));
        assert_eq!(
            state_line["bytes"],
            json!(std::fs::metadata(state_file(&app.cfg.data_dir)).unwrap().len()),
            "the size is the file's, so the CLI can show what it is about to delete"
        );
        assert_eq!(answer["delete"].as_array().unwrap().len(), 1, "no key without --hash-key");
        assert_eq!(listing(&app.cfg.data_dir), before, "nothing went");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A scoped token, however unlimited, never reaches the handler: the route is absent from the
    /// feature's `token_scope`, so `api_tokens::classify` leaves it owner-only. The real guard, the
    /// real router — the same shape the token tests in server.rs drive.
    #[tokio::test]
    async fn a_reset_is_owner_only() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        let made = app
            .api_tokens
            .create(crate::api_tokens::NewToken {
                name: "watcher".into(),
                scope: "read".into(),
                orgs: Vec::new(),
                repos: Vec::new(),
                max_concurrent: None,
                budget_usd_per_day: None,
            })
            .await
            .unwrap();
        let router = crate::server::router(&app);
        let post = |token: &str| {
            router.clone().oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/observability/reset")
                    .header(header::HOST, "127.0.0.1:7878")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
        };
        let res = post(&made.token).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "an unlimited read token cannot reset");
        assert!(
            state_file(&app.cfg.data_dir).is_file(),
            "and nothing was deleted on the way to the refusal"
        );
        let res = post(&app.api_token).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK, "the owner can");
        assert!(!state_file(&app.cfg.data_dir).exists(), "and it happened");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A backfill in flight holds the route off — it reads `state.json` to know where history
    /// ends — and the refusal names what to do about it.
    #[tokio::test]
    async fn a_reset_refuses_while_a_backfill_is_registered() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        supervisor::test_register_backfill(&app.cfg.data_dir);
        let err = reset(State(app.clone()), request(false, false)).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert!(err.message().contains("backfill"), "{}", err.message());
        assert!(state_file(&app.cfg.data_dir).is_file(), "nothing was deleted");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The handshake the route runs under a live supervisor: a `running` child holds the deletion
    /// until the loop has answered the pending entry and stopped it, and the entry is gone once
    /// the route answers, so the next tick may restart the child.
    #[tokio::test]
    async fn a_reset_waits_for_the_supervisor_to_stop_the_child_and_then_lets_go() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        // No supervision loop runs here and the runtime map is empty, so the route settles at once.
        let started = std::time::Instant::now();
        let Json(answer) = reset(State(app.clone()), request(false, false)).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "an empty runtime map settles at once"
        );
        assert_eq!(answer["reset"], json!(true));
        assert!(!pending(&app.cfg.data_dir), "the pending entry is cleared with the answer");
        assert!(!state_file(&app.cfg.data_dir).exists());

        // A `running` child holds the deletion until the loop has stopped it: here a stand-in loop
        // flips the state a moment in, and the route waits for that flip before it deletes.
        supervisor::test_set_runtime(&app.cfg.data_dir, "running");
        std::fs::write(state_file(&app.cfg.data_dir), b"{}").unwrap();
        let data_dir = app.cfg.data_dir.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            supervisor::test_set_runtime(&data_dir, "resetting");
        });
        let started = std::time::Instant::now();
        let Json(answer) = reset(State(app.clone()), request(false, false)).await.unwrap();
        assert_eq!(answer["reset"], json!(true));
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "the route waited for the child to stop, not raced it"
        );
        assert!(!state_file(&app.cfg.data_dir).exists());
        assert!(!pending(&app.cfg.data_dir));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A child that never stops is not something to delete under: past `SETTLE_BY` the route
    /// refuses (409) and leaves `state.json` alone, because a live child holds the cursors in
    /// memory and would write them back over the deletion. The pending entry goes either way, so
    /// the next attempt — or the supervision loop — is not held back. Paused time, so the deadline
    /// is crossed in the test's own microseconds.
    #[tokio::test(start_paused = true)]
    async fn a_reset_refuses_rather_than_delete_under_a_child_that_will_not_stop() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        // A supervision loop that says `running` and never answers.
        supervisor::test_set_runtime(&app.cfg.data_dir, "running");
        let err = reset(State(app.clone()), request(false, false)).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert!(
            err.message().contains("still stopping"),
            "the refusal names what to do: {}",
            err.message()
        );
        assert!(
            state_file(&app.cfg.data_dir).is_file(),
            "nothing was deleted under a child that may still be live"
        );
        assert!(!pending(&app.cfg.data_dir), "and the supervisor is not held back");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Two resets on one data dir would share the pending entry, and the first to answer would
    /// release the supervision loop out from under the second. So the second is refused (409) and
    /// deletes nothing; the entry it did not take is the first one's.
    #[tokio::test]
    async fn a_reset_is_refused_while_another_is_in_flight() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        // A stand-in for the reset in flight: the entry stands for the whole handshake.
        assert!(register(&app.cfg.data_dir));
        assert!(pending(&app.cfg.data_dir));
        let err = reset(State(app.clone()), request(false, false)).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert!(err.message().contains("already in flight"), "{}", err.message());
        assert!(state_file(&app.cfg.data_dir).is_file(), "the refused reset deleted nothing");
        assert!(
            pending(&app.cfg.data_dir),
            "and it did not take the entry that was already standing"
        );
        clear(&app.cfg.data_dir);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A dry run never registers: listing what would go must not hold the supervisor back, even
    /// while a real reset is in flight.
    #[tokio::test]
    async fn a_dry_run_does_not_register_a_pending_entry() {
        let root = crate::tests::temp_root();
        let app = crate::tests::test_app(&root);
        seed(&app.cfg.data_dir);
        assert!(register(&app.cfg.data_dir));
        let Json(answer) = reset(State(app.clone()), request(true, false)).await.unwrap();
        assert_eq!(answer["dry_run"], json!(true), "{answer}");
        assert!(pending(&app.cfg.data_dir), "the entry in flight is untouched");
        clear(&app.cfg.data_dir);
        let _ = std::fs::remove_dir_all(root);
    }
}
