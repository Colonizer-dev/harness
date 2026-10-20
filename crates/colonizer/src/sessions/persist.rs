//! Reading and writing the colony list: `App::session`, `update_session` and the save to
//! `sessions.json`, the per-colony directories and logs.

use std::collections::HashMap;

use super::*;

/// Whether two session records hold the same content, ignoring `updated_at`. Compared through
/// their JSON projections rather than a derived `PartialEq`: floats like `cost_usd` make
/// structural equality brittle, and the projection keeps the check to exactly what persists.
/// A serialization failure falls back to "changed", so a write is never skipped on doubt.
fn session_contents_equal(before: &Session, after: &Session) -> bool {
    let (Ok(Value::Object(mut a)), Ok(Value::Object(mut b))) = (serde_json::to_value(before), serde_json::to_value(after)) else {
        return false;
    };
    a.remove("updated_at");
    b.remove("updated_at");
    a == b
}

impl App {
    /// The index's path in the local working copy (`crate::store::local_index`).
    pub fn sessions_file(&self) -> PathBuf {
        crate::store::local_index(&self.cfg.data_dir)
    }

    /// Every per-task model routing decision, one JSON line each. Kept in the data dir rather than a
    /// session's directory: the record has to outlive cleanup, so the rule can be judged across
    /// colonies instead of disappearing with each one.
    pub(crate) fn routing_file(&self) -> PathBuf {
        self.cfg.data_dir.join("routing.jsonl")
    }

    /// Every Jev visibility-ladder measurement row (#475), one JSON line each: the compaction pass's
    /// per-chunk decisions and the rereads that grade them. Kept in the data dir rather than a
    /// session's directory, like the routing ledger: the measurement has to outlive cleanup and stay
    /// queryable across sessions and colonies, for the bench-wide report (`scripts/bench.mjs jev`).
    pub(crate) fn jev_ladder_file(&self) -> PathBuf {
        self.cfg.data_dir.join("jev_ladder.jsonl")
    }

    /// Every Jev brief-pick measurement (#585), one JSON line each: what a colony's boot offered and
    /// picked, and the notes and packs it was later seen to use. Kept in the data dir like the other
    /// Jev ledgers, for the bench-wide report (`scripts/bench.mjs brief`).
    pub(crate) fn brief_picks_file(&self) -> PathBuf {
        self.cfg.data_dir.join("brief_picks.jsonl")
    }

    /// Every verification's focused-first measurement (#584), one JSON line each, kept in the data
    /// dir like the ladder ledger so the speed-up can be judged across colonies.
    pub(crate) fn jev_focus_file(&self) -> PathBuf {
        self.cfg.data_dir.join("jev_focus.jsonl")
    }

    /// Every Jev decision point's ask (issue #582), one JSON line each: what the point asked, the
    /// pick or why there was none, and what the harness did with it. In the data dir like the routing
    /// ledger, so it outlives per-colony cleanup and spans every point, not just routing.
    pub(crate) fn decisions_file(&self) -> PathBuf {
        self.cfg.data_dir.join("decisions.jsonl")
    }

    /// A session's directory in the local working copy (`crate::store::local_session_dir`): for
    /// what the store API cannot carry — the microVM's mounts (`vm/`, `out/`, `transcripts/`), a
    /// file handed to a subprocess by path, a disk measurement. Reads and writes of the session's
    /// records go through [`App::store`].
    pub fn session_dir(&self, id: &str) -> PathBuf {
        crate::store::local_session_dir(&self.cfg.data_dir, id)
    }

    pub async fn session(&self, id: &str) -> Option<Session> {
        self.sessions.read().await.iter().find(|s| s.id == id).cloned()
    }

    pub async fn update_session<R>(&self, id: &str, f: impl FnOnce(&mut Session) -> R) -> Option<(Session, R)> {
        let (session, returned, result) = {
            let mut sessions = self.sessions.write().await;
            let session = sessions.iter_mut().find(|s| s.id == id)?;
            // Told apart before the closure runs, so the journal can hear once about the crossing
            // into a terminal state — PrOpened, merged, closed, stopped or failed — and never about
            // the later updates inside it. That one hearing is the run's `returned` edge, filed
            // against the day it happened, so it survives the cleanup or delete that forgets the
            // colony itself.
            let was_terminal = session.status.is_terminal();
            let was_failed = session.status == SessionStatus::Failed;
            let before = session.clone();
            let result = f(session);
            if session_contents_equal(&before, session) {
                // The closure changed nothing: leave `updated_at` alone and skip the persist and
                // broadcast, or every no-op poll rewrites sessions.json and wakes every browser.
                // There can be no terminal transition either, so no spend edge is recorded.
                // `updated_at` itself is excluded from the comparison above.
                let session = session.clone();
                return Some((session, result));
            }
            let returned = !was_terminal && session.status.is_terminal();
            // A move into `Failed` from a non-failed status marks the failure unseen (issue #744);
            // updates inside `failed` — a longer message, the reclaim — are not new failures.
            if !was_failed && session.status == SessionStatus::Failed {
                session.unseen_failure = true;
            }
            session.updated_at = Utc::now();
            (session.clone(), (returned, before.status), result)
        };
        let (returned, status_before) = returned;
        if returned {
            spend::record_returned(self, &session).await;
            // The colony's actual dollar cost, next to the decision that routed it (issue #470): the
            // estimate recorded at boot can be checked against this once the colony finishes. Guarded
            // on `model_routing` so a future colony type that skips routing never grows a spurious
            // actual row. A lost row is a lost measurement, not a failed finish, so a failed append
            // only raises the storage alert — the same deal the spend journal and the boot-time
            // decision row get.
            if session.model_routing.is_some() {
                let line = json!({
                    "ts": Utc::now(),
                    "kind": "actual",
                    "session": session.id,
                    "actual_cost_usd": session.total_cost_usd(),
                })
                .to_string();
                if let Err(e) = append_line(&self.routing_file(), &line).await {
                    self.storage_failed("append to the routing ledger", &e).await;
                }
            }
        }
        // The activity log hears the same edge, once: a write that leaves the status alone
        // (cleanup, the app slot, a restart re-marking a stopped colony) records nothing.
        crate::activity::record_transition(self, status_before, &session).await;
        self.persist_and_broadcast(&session).await;
        if returned {
            // The run is over (issue #496): snapshot the logs into the local archive. Spawned, and
            // deaf to failure — a slow or broken archive must never delay or fail a colony that
            // just finished, so the spawned task's whole error path is a printout.
            let data_dir = self.cfg.data_dir.clone();
            let mothership = crate::runtime::host_id(self);
            crate::archive::spawn_on_end(self.store.clone(), data_dir, session.clone(), mothership);
        }
        Some((session, result))
    }

    /// Records a background measurement (e.g. `host_disk_bytes`) without
    /// bumping `updated_at`: a measurement is not activity, and the stall
    /// readout falls back to `updated_at` for colonies with no events yet, so
    /// stamping every watch tick would pin them at "just now" forever.
    /// Persists and broadcasts only when the value actually changed.
    pub async fn record_measurement(&self, id: &str, f: impl FnOnce(&mut Session)) {
        let session = {
            let mut sessions = self.sessions.write().await;
            let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
                return;
            };
            let before = session.clone();
            f(session);
            if session_contents_equal(&before, session) {
                return;
            }
            session.clone()
        };
        self.persist_and_broadcast(&session).await;
    }

    /// Everything `update_session` does after letting go of the write lock: persist the list to disk and
    /// tell every open browser the session changed. Claims made directly under `with_slot` (the queue, a
    /// queued resume) call this too, or the web UI would stop updating.
    pub(crate) async fn persist_and_broadcast(&self, session: &Session) {
        if let Err(e) = self.persist_sessions().await {
            // The change in memory is real, and the broadcast below tells the truth about it —
            // hiding it would make the UI more wrong, not less. But the saved list now lags, so
            // the gap is recorded loudly, in the app alert and in the colony's own log.
            self.storage_failed("save the session list", &e).await;
            self.session_log(&session.id, "error", format!("could not save the session list: {e:#}"))
                .await;
        }
        let rt = self.runtimes.lock().await.get(&session.id).cloned();
        if let Some(rt) = rt {
            let view = with_activity(self, session.clone()).await;
            rt.broadcast(None, json!({"type": "session", "session": view}).to_string());
        }
    }

    pub(crate) async fn persist_sessions(&self) -> Result<()> {
        let _guard = self.session_persist.lock().await;
        let data = serde_json::to_vec_pretty(&*self.sessions.read().await).context("could not serialize the session list")?;
        // Through the session store this App was built with (the default local one), not a fresh
        // file helper: the path and bytes are identical, and the write answers by the same name
        // every backend will answer it by.
        self.store().write_index(&data).await?;
        // The session list is written on nearly every state change, so its saves are the signal
        // that the disk is taking writes again after a failure.
        self.storage_succeeded().await;
        Ok(())
    }

    /// Issue #1284: pure function computing canonical casings for sessions (testable).
    /// Prefers saved org settings keys; unknown orgs use majority casing among sessions.
    /// Repo = "{org}/{name}" where both are canonical. Name casings unified per
    /// lowercased owner/name (majority, ties lexicographically smallest).
    /// Safety: skips sessions with empty org or repo owner ≠ org (case-insensitive).
    /// Returns the count of sessions changed.
    pub fn canonicalise_casing(sessions: &mut [Session], saved_orgs: &[String]) -> usize {
        // Compute canonical org casings: saved keys first, then majority from sessions.
        // Skip empty orgs when counting.
        let mut canonical_orgs: HashMap<String, String> = HashMap::new();
        for key in saved_orgs {
            canonical_orgs.insert(key.to_ascii_lowercase(), key.clone());
        }
        let mut org_casings: HashMap<String, HashMap<String, usize>> = HashMap::new();
        for s in sessions.iter() {
            if s.org.is_empty() {
                continue;
            }
            let lower = s.org.to_ascii_lowercase();
            if !canonical_orgs.contains_key(&lower) {
                *org_casings.entry(lower).or_default().entry(s.org.clone()).or_insert(0) += 1;
            }
        }
        for (lower, casings) in org_casings {
            let lower_copy = lower.clone();
            canonical_orgs.entry(lower).or_insert_with(|| {
                casings
                    .into_iter()
                    .max_by(|(a, ca), (b, cb)| ca.cmp(cb).then_with(|| b.cmp(a)))
                    .map(|(casing, _)| casing)
                    .unwrap_or_else(|| lower_copy.clone())
            });
        }

        // Pre-compute canonical name casings per lowercase owner/name to avoid duplication.
        let mut name_winners: HashMap<String, String> = HashMap::new();
        {
            let mut name_casings: HashMap<String, HashMap<String, usize>> = HashMap::new();
            for s in sessions.iter() {
                if let Some((owner, name)) = s.repo.split_once('/') {
                    let key = format!("{}/{}", owner.to_ascii_lowercase(), name.to_ascii_lowercase());
                    *name_casings.entry(key).or_default().entry(name.to_string()).or_insert(0) += 1;
                }
            }
            for (key, casings) in name_casings {
                let key_copy = key.clone();
                name_winners.insert(
                    key,
                    casings
                        .into_iter()
                        .max_by(|(a, ca), (b, cb)| ca.cmp(cb).then_with(|| b.cmp(a)))
                        .map(|(n, _)| n)
                        .unwrap_or_else(|| key_copy.split('/').nth(1).unwrap_or("").to_string()),
                );
            }
        }

        // Apply canonicalization: org and repo owner/name where both are canonical.
        // Safety: skip sessions with empty org or repo owner ≠ org (case-insensitive).
        let mut count_changed = 0;
        for session in sessions.iter_mut() {
            if session.org.is_empty() {
                continue;
            }

            let old_org = session.org.clone();
            let old_repo = session.repo.clone();
            let lower_org = old_org.to_ascii_lowercase();

            // Canonicalize org.
            if let Some(canonical) = canonical_orgs.get(&lower_org) {
                session.org = canonical.clone();
            }

            // Canonicalize repo: only if owner matches org (case-insensitive) and repo has '/'.
            if let Some((owner, name)) = old_repo.split_once('/')
                && owner.eq_ignore_ascii_case(&old_org)
            {
                let key = format!("{}/{}", lower_org, name.to_ascii_lowercase());
                let canonical_name = name_winners.get(&key).cloned().unwrap_or_else(|| name.to_string());
                session.repo = format!("{}/{}", session.org, canonical_name);
            }

            if session.org != old_org || session.repo != old_repo {
                count_changed += 1;
            }
        }

        count_changed
    }

    /// Issue #1284: migrate session casings to canonical forms. Calls `canonicalise_casing()`
    /// and persists only if anything changed.
    pub(crate) async fn migrate_session_casings(&self) -> usize {
        let saved_orgs: Vec<String> = self.all_org_settings().keys().cloned().collect();
        let mut sessions = self.sessions.write().await;
        let count_changed = Self::canonicalise_casing(&mut sessions, &saved_orgs);
        drop(sessions);

        // Persist and log if anything changed.
        if count_changed > 0 {
            if let Err(e) = self.persist_sessions().await {
                tracing::warn!(changed = count_changed, error = %e, "migrate_session_casings: could not persist: {e:#}");
            } else {
                tracing::info!(
                    changed = count_changed,
                    "migrated {} sessions to canonical casings",
                    count_changed
                );
            }
        }

        count_changed
    }

    pub async fn runtime(&self, id: &str) -> Arc<Runtime> {
        if let Some(rt) = self.runtimes.lock().await.get(id) {
            return rt.clone();
        }
        // Read through the store outside the map's lock, then insert: two first callers racing both
        // build one, and the first insert wins, so every caller still shares a single runtime. A
        // resume never empties the map around its rotation (`replace_runtime`), so a build that read
        // the log before the rename loses this insert to the runtime rebuilt after it.
        let built = self.build_runtime(id).await;
        self.runtimes.lock().await.entry(id.to_string()).or_insert(built).clone()
    }

    /// Puts a runtime rebuilt from the colony's saved logs in place of its current one, answering the
    /// one it replaced: the swap `lifecycle::retire_and_rotate_events` makes once the log is rotated.
    pub(crate) async fn replace_runtime(&self, id: &str) -> Option<Arc<Runtime>> {
        let built = self.build_runtime(id).await;
        self.runtimes.lock().await.insert(id.to_string(), built)
    }

    async fn build_runtime(&self, id: &str) -> Arc<Runtime> {
        let saved = SavedLogs::read(self.store(), id).await;
        Arc::new(Runtime::from_saved(saved, &self.session_dir(id).join("out")))
    }

    /// The colony log, stamped `system`: the host's own bookkeeping. A subsystem that speaks in its
    /// own voice — the watchdog's nudges, the judge's narration, a burn-down launch, a notification
    /// failure — logs through [`App::session_log_as`] so the line names what caused it (§3).
    pub async fn session_log(&self, id: &str, level: &str, message: String) {
        self.session_log_as(Origin::System, id, level, message).await
    }

    /// [`App::session_log`] with the `origin` the line is stamped with (docs/protocol.md §3).
    pub(crate) async fn session_log_as(&self, origin: Origin, id: &str, level: &str, message: String) {
        // #761: a message quoting a command line, an error body or a URL can carry a credential.
        let message = crate::redact::redact_text(&message).into_owned();
        let entry =
            json!({"type": "harness_log", "origin": origin.as_str(), "level": level, "message": message, "ts": Utc::now()});
        let rt = self.runtime(id).await;
        let persisted = {
            let _guard = rt.file_lock.lock().await;
            // Through the store, so the line lands in `sessions/<id>/harness.jsonl` by the same name
            // a remote backend would answer by. The store adds the newline, as `append_line` did.
            let line = entry.to_string();
            self.store()
                .append(id, "harness.jsonl", line.as_bytes())
                .await
                .map_err(anyhow::Error::from)
                .err()
        };
        if let Some(e) = persisted {
            // Recorded here, not by calling session_log again — that would recurse — and outside
            // the guard, as in handle_agent_event. The frame still reaches every open browser
            // below; the console and the app alert keep the gap.
            tracing::error!(
                id = %id,
                error = %format!("{e:#}"),
                "sessions: could not append to colony {id}'s harness.jsonl: {e:#}"
            );
            self.storage_failed("append to the harness log", &e).await;
        }
        let mut logs = rt.logs.lock().await;
        logs.push_back(entry.clone());
        while logs.len() > MAX_LOGS {
            logs.pop_front();
        }
        rt.broadcast(None, entry.to_string());
    }

    /// Notes an attention flag a terminal transition just cleared in the colony's log, so the
    /// reason it was set survives the clear. Silent when there was no flag.
    pub(crate) async fn note_cleared_attention(&self, id: &str, attention: Option<Value>) {
        if let Some(message) = cleared_attention_message(&attention) {
            self.session_log(id, "info", message).await;
        }
    }

    /// Reports a read failure from `Runtime::load`, once, the first time the colony's runtime is
    /// actually used. Not done inside `runtime()`: `session_log` calls back into it, and the
    /// runtimes map is locked there. The message is built per file in `load` — this only puts it
    /// on the record.
    pub(crate) async fn report_load_error(&self, id: &str, rt: &Runtime) {
        let Some(message) = rt.load_error.lock().await.take() else {
            return;
        };
        let err = anyhow::Error::msg(message.clone());
        self.storage_failed("read the colony's saved history", &err).await;
        self.session_log(id, "error", message).await;
    }

    pub(crate) fn logger(self: &Arc<Self>, id: &str) -> SessionLogger {
        SessionLogger {
            app: self.clone(),
            id: id.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::util::faults::{self, Op};

    use super::*;
    use crate::sessions::tests::*;

    #[tokio::test]
    async fn a_failed_save_is_alerted_and_the_colony_log_shows_the_gap() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let _guard = faults::inject("sessions.json", Op::Write, || {
            std::io::Error::from(std::io::ErrorKind::StorageFull)
        });
        let (s, ()) = app.update_session("abc", |s| s.status = SessionStatus::Idle).await.unwrap();
        assert_eq!(
            s.status,
            SessionStatus::Idle,
            "the in-memory change is kept and still broadcast"
        );
        let alert = app.storage_alert.read().await.clone().unwrap();
        assert!(alert.message.contains("save the session list"), "{}", alert.message);
        let log = std::fs::read_to_string(app.session_dir("abc").join("harness.jsonl")).unwrap();
        assert!(log.contains("could not save the session list"), "{log}");
        drop(_guard);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn update_session_with_noop_closure_leaves_updated_at_and_disk_alone() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let before = app.session("abc").await.unwrap();
        // Seed sessions.json so a write would be observable.
        app.persist_sessions().await.unwrap();
        let disk_before = std::fs::read_to_string(app.sessions_file()).unwrap();
        // updated_at only has second precision on disk in spirit; sleep past any clock bump so
        // a spurious bump could not hide behind timestamp granularity.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let (after, ()) = app.update_session("abc", |_| {}).await.unwrap();
        assert_eq!(
            after.updated_at, before.updated_at,
            "a closure that changes nothing must not bump updated_at"
        );
        let disk_after = std::fs::read_to_string(app.sessions_file()).unwrap();
        assert_eq!(disk_after, disk_before, "a no-op update must not rewrite sessions.json");
        assert!(
            app.storage_alert.read().await.is_none(),
            "skipping the write must not raise a storage alert"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn update_session_with_real_change_bumps_updated_at_and_persists() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let before = app.session("abc").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let (after, ()) = app.update_session("abc", |s| s.status = SessionStatus::Idle).await.unwrap();
        assert_eq!(after.status, SessionStatus::Idle);
        assert!(after.updated_at > before.updated_at, "a real change must bump updated_at");
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(app.sessions_file()).unwrap()).unwrap();
        let saved = disk.as_array().unwrap().iter().find(|v| v["id"] == "abc").unwrap();
        assert_eq!(saved["status"], "idle", "a real change must reach sessions.json");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The routing ledger's `actual` row (issue #470): when a routed colony crosses into a terminal
    /// state its real dollar cost lands next to the boot-time decision, so the estimate can be
    /// checked against reality. A colony that never went through routing gets no such row.
    #[tokio::test]
    async fn a_colony_crossing_to_terminal_appends_its_actual_cost_to_the_routing_ledger() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        app.update_session("abc", |s| s.model_routing = Some(json!({"tier": "low"})))
            .await;
        app.update_session("abc", |s| s.status = SessionStatus::Merged).await;
        let ledger = |app: &Shared| -> Vec<Value> {
            std::fs::read_to_string(app.routing_file())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        };
        let rows = ledger(&app);
        assert_eq!(rows.len(), 1, "one ledger line, the actual cost: {rows:?}");
        assert_eq!(rows[0]["kind"], "actual");
        assert_eq!(rows[0]["session"], "abc");
        assert!(rows[0]["ts"].is_string(), "{}", rows[0]);
        assert!(rows[0]["actual_cost_usd"].is_number(), "{}", rows[0]);

        // No routing decision, no actual row — even though the same returned edge fires for it.
        let mut other = colony("acme", SessionStatus::Running);
        other.id = "def".into();
        app.sessions.write().await.push(other);
        app.update_session("def", |s| s.status = SessionStatus::Merged).await;
        assert_eq!(ledger(&app).len(), 1, "a colony that skipped routing gets no actual row");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #744: the crossing into `Failed` marks the failure unseen — that is what the app
    /// badge counts until `POST /api/sessions/{id}/seen` clears it. Later updates inside
    /// `failed` are not new failures, and a terminal move that is not a failure marks nothing.
    #[tokio::test]
    async fn moving_into_failed_marks_the_failure_unseen() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        assert!(!app.session("abc").await.unwrap().unseen_failure);
        app.update_session("abc", |s| s.status = SessionStatus::Failed).await.unwrap();
        assert!(app.session("abc").await.unwrap().unseen_failure, "the crossing is unseen");
        app.update_session("abc", |s| s.unseen_failure = false).await.unwrap();
        app.update_session("abc", |s| s.error = Some("a longer message".into()))
            .await
            .unwrap();
        assert!(
            !app.session("abc").await.unwrap().unseen_failure,
            "a later update inside `failed` is not a new failure"
        );
        let (other, other_root) = app_with_colony("def", SessionStatus::Running).await;
        other
            .update_session("def", |s| s.status = SessionStatus::Stopped)
            .await
            .unwrap();
        assert!(!other.session("def").await.unwrap().unseen_failure, "stopping is not failing");
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other_root);
    }

    #[tokio::test]
    async fn record_measurement_stores_the_number_without_bumping_updated_at() {
        let (app, root) = app_with_colony("abc", SessionStatus::Running).await;
        let before = app.session("abc").await.unwrap();
        assert_eq!(before.host_disk_bytes, None);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        app.record_measurement("abc", |s| s.host_disk_bytes = Some(1024)).await;
        let after = app.session("abc").await.unwrap();
        assert_eq!(after.host_disk_bytes, Some(1024));
        assert_eq!(
            after.updated_at, before.updated_at,
            "a measurement is not activity and must not read as liveness"
        );
        let disk: Value = serde_json::from_str(&std::fs::read_to_string(app.sessions_file()).unwrap()).unwrap();
        let saved = disk.as_array().unwrap().iter().find(|v| v["id"] == "abc").unwrap();
        assert_eq!(
            saved["host_disk_bytes"], 1024,
            "the measurement must still reach sessions.json"
        );
        // A second identical measurement writes nothing.
        let disk_before = std::fs::read_to_string(app.sessions_file()).unwrap();
        app.record_measurement("abc", |s| s.host_disk_bytes = Some(1024)).await;
        assert_eq!(
            std::fs::read_to_string(app.sessions_file()).unwrap(),
            disk_before,
            "an unchanged measurement must not rewrite sessions.json"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Issue #1284: canonicalise_casing prefers saved org keys. Invariant:
    /// every session's repo owner = org (case-equal), and case-equal orgs/names unified.
    /// Safety: sessions with empty org or mismatched repo owner are skipped.
    #[test]
    fn canonicalise_casing_prefers_saved_org_key() {
        let mut sessions = [
            {
                let mut s = colony("acme", SessionStatus::Running);
                s.repo = "acme/myapp".to_string();
                s
            },
            {
                let mut s = colony("ACME", SessionStatus::Running);
                s.repo = "ACME/myapp".to_string();
                s.id = "s2".to_string();
                s
            },
            {
                // Safety: empty org, repo should not be rewritten
                let mut s = colony("", SessionStatus::Running);
                s.repo = "other/app".to_string();
                s.id = "s3".to_string();
                s
            },
            {
                // Safety: repo owner ≠ org (case-insensitive), repo should not be rewritten
                let mut s = colony("acme", SessionStatus::Running);
                s.repo = "OTHER/app".to_string();
                s.id = "s4".to_string();
                s
            },
        ];
        let saved_orgs = vec!["Acme".to_string()];
        let changed = App::canonicalise_casing(&mut sessions, &saved_orgs);
        assert_eq!(
            changed, 3,
            "s0, s1, s3 changed (orgs canonicalized); s2 and s4 repos skipped for safety"
        );
        // Saved key "Acme" wins over any majority; both org and repo owner unified to "Acme".
        assert_eq!(sessions[0].org, "Acme");
        assert_eq!(sessions[1].org, "Acme");
        assert_eq!(sessions[0].repo, "Acme/myapp");
        assert_eq!(sessions[1].repo, "Acme/myapp");
        // Safety: sessions with empty org or mismatched owner skip repo rewriting but org still canonicalized.
        assert_eq!(sessions[2].org, "", "empty org unchanged (skipped entirely)");
        assert_eq!(sessions[2].repo, "other/app", "repo with empty org untouched");
        assert_eq!(sessions[3].org, "Acme", "org canonicalized even when repo owner mismatches");
        assert_eq!(
            sessions[3].repo, "OTHER/app",
            "repo untouched when owner ≠ org (case-insensitive)"
        );
        // Verify invariant for safe sessions: repo owner equals org.
        for s in &sessions[..2] {
            assert_eq!(s.repo.split('/').next(), Some(s.org.as_str()), "{} invariant", s.id);
        }
    }

    /// Issue #1284: canonicalise_casing uses majority for unknown orgs/names, with
    /// tie-break (lexicographically smallest). Invariant: repo owner = org exactly.
    #[test]
    fn canonicalise_casing_uses_majority_for_unknown_orgs() {
        let mut sessions = [
            {
                let mut s = colony("acme", SessionStatus::Running);
                s.repo = "acme/app".to_string();
                s
            },
            {
                let mut s = colony("ACME", SessionStatus::Running);
                s.repo = "acme/app".to_string();
                s.id = "s2".to_string();
                s
            },
            {
                let mut s = colony("ACME", SessionStatus::Running);
                s.repo = "acme/App".to_string();
                s.id = "s3".to_string();
                s
            },
            {
                let mut s = colony("ACME", SessionStatus::Running);
                s.repo = "acme/other".to_string();
                s.id = "s4".to_string();
                s
            },
            {
                let mut s = colony("ACME", SessionStatus::Running);
                s.repo = "acme/Other".to_string();
                s.id = "s5".to_string();
                s
            },
        ];
        let saved_orgs = vec![];
        let changed = App::canonicalise_casing(&mut sessions, &saved_orgs);
        // org majority: acme 1x, ACME 4x -> ACME.
        // name "app" majority: "app" 2x, "App" 1x -> "app".
        // name "other" tie: "other" 1x, "Other" 1x -> "Other" (lexicographically smallest).
        assert_eq!(changed, 5, "all 5 sessions changed (org/repo canonicalized)");
        for s in &sessions {
            assert_eq!(s.org, "ACME", "{}: org should be ACME", s.id);
            assert!(s.repo.starts_with("ACME/"), "{}: repo owner should be ACME", s.id);
            assert_eq!(s.repo.split('/').next(), Some(s.org.as_str()), "{}: invariant", s.id);
        }
        // Verify name canonicalizations.
        assert_eq!(sessions[0].repo, "ACME/app", "s1 name 'app' wins (majority)");
        assert_eq!(sessions[1].repo, "ACME/app");
        assert_eq!(sessions[2].repo, "ACME/app");
        assert_eq!(sessions[3].repo, "ACME/Other", "s4/s5 name 'Other' wins (tie-break)");
        assert_eq!(sessions[4].repo, "ACME/Other");
    }

    /// Issue #1284: canonicalise_casing returns 0 and changes nothing when already canonical.
    #[test]
    fn canonicalise_casing_noop_when_canonical() {
        let mut sessions = [
            {
                let mut s = colony("Acme", SessionStatus::Running);
                s.repo = "Acme/app".to_string();
                s
            },
            {
                let mut s = colony("Acme", SessionStatus::Running);
                s.repo = "Acme/app".to_string();
                s.id = "s2".to_string();
                s
            },
        ];
        let saved_orgs = vec!["Acme".to_string()];
        let changed = App::canonicalise_casing(&mut sessions, &saved_orgs);
        assert_eq!(changed, 0, "no changes when already canonical");
        assert_eq!(sessions[0].org, "Acme");
        assert_eq!(sessions[1].org, "Acme");
        assert_eq!(sessions[0].repo, "Acme/app");
        assert_eq!(sessions[1].repo, "Acme/app");
        // Verify invariant.
        for s in &sessions {
            assert_eq!(s.repo.split('/').next(), Some(s.org.as_str()), "{} invariant", s.id);
        }
    }
}
