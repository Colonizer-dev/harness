//! Reading and writing the colony list: `App::session`, `update_session` and the save to
//! `sessions.json`, the per-colony directories and logs.

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
    pub fn sessions_file(&self) -> PathBuf {
        self.cfg.data_dir.join("sessions.json")
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
    /// queryable across sessions and colonies, for stage 2's bench-wide precision/recall report.
    pub(crate) fn jev_ladder_file(&self) -> PathBuf {
        self.cfg.data_dir.join("jev_ladder.jsonl")
    }

    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.cfg.data_dir.join("sessions").join(id)
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
            crate::archive::spawn_on_end(data_dir, session.clone(), mothership);
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
        // Through the session store (the default local one), not the file helper directly: the
        // path and bytes are identical, and the write goes by the same name every backend will
        // answer it by.
        crate::store::LocalDirStore::new(self.cfg.data_dir.clone())
            .write_index(&data)
            .await?;
        // The session list is written on nearly every state change, so its saves are the signal
        // that the disk is taking writes again after a failure.
        self.storage_succeeded().await;
        Ok(())
    }

    pub async fn runtime(&self, id: &str) -> Arc<Runtime> {
        let mut runtimes = self.runtimes.lock().await;
        runtimes
            .entry(id.to_string())
            .or_insert_with(|| Arc::new(Runtime::load(&self.session_dir(id))))
            .clone()
    }

    /// The colony log, stamped `system`: the host's own bookkeeping. A subsystem that speaks in its
    /// own voice — the watchdog's nudges, the judge's narration, a burn-down launch, a notification
    /// failure — logs through [`App::session_log_as`] so the line names what caused it (§3).
    pub async fn session_log(&self, id: &str, level: &str, message: String) {
        self.session_log_as(Origin::System, id, level, message).await
    }

    /// [`App::session_log`] with the `origin` the line is stamped with (docs/protocol.md §3).
    pub(crate) async fn session_log_as(&self, origin: Origin, id: &str, level: &str, message: String) {
        let entry =
            json!({"type": "harness_log", "origin": origin.as_str(), "level": level, "message": message, "ts": Utc::now()});
        let rt = self.runtime(id).await;
        let persisted = {
            let _guard = rt.file_lock.lock().await;
            append_line(&rt.logs_path, &entry.to_string()).await.err()
        };
        if let Some(e) = persisted {
            // Recorded here, not by calling session_log again — that would recurse — and outside
            // the guard, as in handle_agent_event. The frame still reaches every open browser
            // below; the console and the app alert keep the gap.
            eprintln!("sessions: could not append to {}: {e:#}", rt.logs_path.display());
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
}
