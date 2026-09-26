//! A live colony's in-memory side: the event fan-out to browsers, the agent link, and the
//! session-scoped harness log.

use super::*;

/// In-memory state for a session's event fan-out and agent link.
pub struct Runtime {
    pub(crate) events: broadcast::Sender<Arc<Broadcast>>,
    pub(crate) commands: mpsc::UnboundedSender<Value>,
    pub(crate) commands_rx: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
    /// The last *agentd* seq persisted, and only agentd events advance it: it is the dedupe cursor
    /// the reconnect guard compares against (`events.rs` `handle_agent_event`) and the `?since=`
    /// rank agentd replays from. Host chain events (`validation.rs` `emit_chain`) live in the file's
    /// seq space but never move this cursor, so one cursor cannot push the other past an event that
    /// has not arrived yet — the shared cursor that used to do both is exactly what dropped the next
    /// real agentd event when a host chain event consumed its rank.
    pub(crate) agent_seq: AtomicU64,
    /// The highest `seq` written to `events.jsonl` so far, agentd and host chain lines together. It
    /// is the monotonic rank a reconnecting browser replays above, and the file's seq at this rank
    /// the browser uses as its own `?since=`. Agentd events whose own seq would regress it are
    /// renumbered to one past it (with their true seq kept in `a_seq`), so the file never holds two
    /// lines out of order.
    pub(crate) last_seq: AtomicU64,
    pub(crate) logs: Mutex<VecDeque<Value>>,
    /// The open question: its id, the questions themselves — which autonomous mode needs to answer
    /// among the options the agent offered — and the question's risk class, which its ceiling reads.
    pub(crate) open_question: Mutex<Option<(String, Vec<Value>, QuestionRisk)>>,
    /// Question ids the autonomy judge has sent an `answer` for and whose `question_answered` echo
    /// has not come back yet (autonomy.rs writes, events.rs spends one entry resolving that echo's
    /// origin). In memory only: after a mothership restart the set is empty, so a judge answer still
    /// in flight reads as the person's — a mislabelled line, never a wrong decision.
    pub(crate) judged_questions: Mutex<HashSet<String>>,
    /// Jev visibility-ladder watch state (#475, jev_ladder.rs): what each recent tool call looked
    /// like, and which pending decisions still await their reread. In memory only, like
    /// `judged_questions`: after a mothership restart the watchlist is empty, so rereads that would
    /// have landed after it are simply not counted — a lost measurement, never a wrong one.
    pub(crate) jev_ladder: Mutex<crate::jev_ladder::Watch>,
    /// `pr.md` as of the last turn end, so autopilot publishes only when a turn wrote it.
    pub(crate) pr_mark: Mutex<Option<(std::time::SystemTime, u64)>>,
    pub(crate) interrupted: std::sync::atomic::AtomicBool,
    /// Set once a run has been told its agent cannot resume a session, so the queue's suspension
    /// tick says so once instead of every 5 s (issue #562). In memory like the other cursors: a
    /// restart saying it again is a minor repeat, a per-tick drumbeat is the leak.
    pub(crate) suspend_skip_logged: std::sync::atomic::AtomicBool,
    pub(crate) stop: watch::Sender<bool>,
    /// Set once, by `resume` on the retired run's Runtime only: pre-existing event sockets hold
    /// that Runtime and can never see the new run's events, so they close and reconnect into the
    /// new epoch. Distinct from `stop`, which `teardown_vm` also sets on a plain stop where
    /// sockets stay open on purpose.
    pub(crate) retired: watch::Sender<bool>,
    pub(crate) file_lock: Mutex<()>,
    /// Serialises findings, so the per-colony cap holds when two arrive together.
    pub(crate) findings_lock: Mutex<()>,
    /// Serialises completion-claim verifications (verify.rs): a second claim that lands mid-run
    /// queues behind it and then verifies the newer state, never concurrent with it.
    pub(crate) verify_lock: Mutex<()>,
    /// Path-policy violations already warned about, so a publish retry — which re-stages the same
    /// tree — does not repeat every line (issue #300). In memory on purpose: a restart warning
    /// again is a minor repeat, a leak-free set is the point.
    pub(crate) path_policy_warned: Mutex<HashSet<String>>,
    pub(crate) events_path: PathBuf,
    pub(crate) logs_path: PathBuf,
    pub activity: Mutex<Activity>,
    /// A read failure from `load`, already worded to name the file and the consequence that
    /// restarts, carried until the first caller that has an `App` to report it with. Leaving it
    /// silent is what issue #107 was about.
    pub(crate) load_error: Mutex<Option<String>>,
}

/// Reads a JSONL file as raw bytes, leaving UTF-8 decoding to the caller's per-line pass. A file
/// that is not there is not a failure — a colony that has never emitted an event has no
/// `events.jsonl` — but anything else is handed back for the caller to report.
fn read_jsonl(path: &std::path::Path) -> (Vec<u8>, Option<std::io::Error>) {
    match std::fs::read(path) {
        Ok(bytes) => (bytes, None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
        Err(e) => (Vec::new(), Some(e)),
    }
}

pub(crate) struct Broadcast {
    pub(crate) seq: Option<u64>,
    pub(crate) json: String,
}

impl Runtime {
    /// The question a colony is waiting on, if it is waiting on one, with its risk class.
    pub(crate) async fn open_question(&self) -> Option<(String, Vec<Value>, QuestionRisk)> {
        self.open_question.lock().await.clone()
    }

    pub(crate) fn load(dir: &std::path::Path) -> Self {
        let events_path = dir.join("events.jsonl");
        let logs_path = dir.join("harness.jsonl");
        let (events_bytes, events_err) = read_jsonl(&events_path);
        // Decoded one line at a time, as agentd reads its own store (colonizer-agentd/src/store.rs): a
        // final line torn inside a multi-byte character then costs that line and not the whole file. A
        // whole-file failure here would silently reset the reconnect cursor, and the colony would replay
        // and duplicate its entire transcript.
        let last_seq = events_bytes
            .split(|b| *b == b'\n')
            .rev()
            .find_map(|line| serde_json::from_str::<Value>(std::str::from_utf8(line).ok()?).ok()?["seq"].as_u64())
            .unwrap_or(0);
        // The reconnect cursor is agentd's, not the file's: only lines the runner wrote count, each
        // at its own seq (a line `handle_agent_event` renumbered because it collided with a host
        // chain event keeps its true seq in `a_seq`). Host chain events are cut out by their type —
        // the seven this build emits and the protocol reserves — so a restart mid-life asks agentd to
        // replay exactly the events it has missed, and cannot skip the ones that never landed.
        //
        // The same pass restores the open question. It is otherwise set only while live events are
        // handled (`events.rs`), and agentd replays only what is past the cursor, so after a restart a
        // question asked before it would be forgotten: the judge would never answer it and autopilot
        // would publish over it. The last question with no later `question_answered` for its id is
        // still open, with its own timestamp as the start of the wait.
        let mut agent_seq = 0;
        // The replayed open question: its id, its questions, when it was asked, its risk class.
        type Replayed = (String, Vec<Value>, Option<DateTime<Utc>>, QuestionRisk);
        let mut open_question: Option<Replayed> = None;
        for v in events_bytes
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_str::<Value>(std::str::from_utf8(line).ok()?).ok())
        {
            let Some(kind) = v.get("type").and_then(Value::as_str) else {
                continue;
            };
            if crate::validation::is_host_chain_type(kind) {
                continue;
            }
            if let Some(seq) = v.get("a_seq").and_then(Value::as_u64).or_else(|| v["seq"].as_u64()) {
                agent_seq = agent_seq.max(seq);
            }
            let question_id = v.get("question_id").and_then(Value::as_str);
            match (kind, question_id) {
                ("question", Some(id)) => {
                    let questions = v.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
                    let asked = v
                        .get("ts")
                        .and_then(Value::as_str)
                        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                        .map(|ts| ts.with_timezone(&Utc));
                    let risk = QuestionRisk::from_wire(v.get("risk"));
                    open_question = Some((id.to_string(), questions, asked, risk));
                }
                ("question_answered", Some(id)) if open_question.as_ref().is_some_and(|(open, ..)| open == id) => {
                    open_question = None;
                }
                _ => {}
            }
        }
        let (logs_bytes, logs_err) = read_jsonl(&logs_path);
        // The take counts parsed entries, not raw split segments: `append_line` ends every entry
        // with a newline, so a well-formed file always yields one empty trailing segment, and
        // taking segments first would keep one entry too few.
        let logs: VecDeque<Value> = logs_bytes
            .split(|b| *b == b'\n')
            .rev()
            .filter_map(|line| serde_json::from_str(std::str::from_utf8(line).ok()?).ok())
            .take(MAX_LOGS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        // Each file's message names its own consequence: only a failed events.jsonl restarts the
        // reconnect cursor, only a failed harness.jsonl restarts the log ring.
        let mut read_errors = Vec::new();
        if let Some(e) = events_err {
            read_errors.push(format!(
                "could not read the saved events ({}: {e}); \
                 the reconnect cursor restarts, so events already on disk may be recorded a second time",
                events_path.display()
            ));
        }
        if let Some(e) = logs_err {
            read_errors.push(format!(
                "could not read the saved colony log ({}: {e}); the log history starts over",
                logs_path.display()
            ));
        }
        let (commands, commands_rx) = mpsc::unbounded_channel();
        Self {
            events: broadcast::channel(1024).0,
            commands,
            commands_rx: Mutex::new(Some(commands_rx)),
            agent_seq: AtomicU64::new(agent_seq),
            last_seq: AtomicU64::new(last_seq),
            logs: Mutex::new(logs),
            open_question: Mutex::new(
                open_question
                    .as_ref()
                    .map(|(id, questions, _, risk)| (id.clone(), questions.clone(), *risk)),
            ),
            judged_questions: Mutex::new(HashSet::new()),
            jev_ladder: Mutex::new(crate::jev_ladder::Watch::default()),
            pr_mark: Mutex::new(github::pr_description_mark(&dir.join("out"))),
            interrupted: std::sync::atomic::AtomicBool::new(false),
            suspend_skip_logged: std::sync::atomic::AtomicBool::new(false),
            stop: watch::channel(false).0,
            retired: watch::channel(false).0,
            file_lock: Mutex::new(()),
            findings_lock: Mutex::new(()),
            verify_lock: Mutex::new(()),
            path_policy_warned: Mutex::new(HashSet::new()),
            events_path,
            logs_path,
            activity: Mutex::new({
                let now = Utc::now();
                let mut activity = Activity::new(now);
                // A question with no readable timestamp starts its wait now, as the live path does.
                activity.question_since = open_question.map(|(_, _, asked, _)| asked.unwrap_or(now));
                activity
            }),
            load_error: Mutex::new((!read_errors.is_empty()).then_some(read_errors.join("; "))),
        }
    }

    /// Records a path-policy warning and says whether it is new, so the colony log carries each
    /// violation once however many publish attempts re-stage the same tree.
    pub(crate) async fn warn_path_policy_once(&self, message: &str) -> bool {
        let mut seen = self.path_policy_warned.lock().await;
        seen.insert(message.to_string())
    }

    pub(crate) fn broadcast(&self, seq: Option<u64>, json: String) {
        let _ = self.events.send(Arc::new(Broadcast { seq, json }));
    }

    /// Queues a command for the agent (sent once the agent link is connected).
    pub fn send_command(&self, command: Value) {
        let _ = self.commands.send(command);
    }
}

/// A session as shown to browsers: live colonies carry their last agent activity.
pub(crate) async fn with_activity(app: &App, mut session: Session) -> Session {
    if session.status.is_live() {
        let rt = app.runtimes.lock().await.get(&session.id).cloned();
        if let Some(rt) = rt {
            session.last_activity_at = Some(rt.activity.lock().await.last);
        }
    }
    session
}

/// Session-scoped harness log (shown in the UI next to agent events).
pub struct SessionLogger {
    pub(super) app: Shared,
    pub(super) id: String,
}

impl SessionLogger {
    pub async fn info(&self, message: impl Into<String>) {
        self.app.session_log(&self.id, "info", message.into()).await
    }
    pub async fn error(&self, message: impl Into<String>) {
        self.app.session_log(&self.id, "error", message.into()).await
    }
    pub async fn warn(&self, message: impl Into<String>) {
        self.app.session_log(&self.id, "warn", message.into()).await
    }
}

#[cfg(test)]
mod tests {
    use crate::util::faults::{self, Op};

    use super::*;
    use crate::sessions::tests::*;

    #[tokio::test]
    async fn a_failed_harness_log_append_still_reaches_the_browser_and_alerts() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let rt = app.runtime("abc").await;
        let _guard = faults::inject("harness.jsonl", Op::Append, || {
            std::io::Error::from(std::io::ErrorKind::PermissionDenied)
        });
        app.session_log("abc", "error", "a message".into()).await;
        assert!(
            app.storage_alert.read().await.is_some(),
            "the failure is recorded, not swallowed"
        );
        {
            let logs = rt.logs.lock().await;
            let last = logs.back().unwrap();
            assert_eq!(last["type"], "harness_log");
            assert_eq!(last["message"], "a message", "the frame still goes out to open browsers");
        }
        drop(_guard);
        app.session_log("abc", "info", "recovered".into()).await;
        assert!(
            app.session_dir("abc").join("harness.jsonl").exists(),
            "appends work again once the fault clears"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_failed_event_append_does_not_advance_last_seq_and_the_lost_line_stays_lost() {
        // (Injecting a fault only on the first append would promise a retry; in reality the next
        // event succeeds and seq 1 is gone for good, which is what this pins.)
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        let rt = app.runtime("abc").await;
        let _guard = faults::inject("events.jsonl", Op::Append, || std::io::Error::from_raw_os_error(5));
        handle_agent_event(&app, "abc", &rt, r#"{"seq":1,"type":"status","state":"working"}"#).await;
        assert!(
            app.storage_alert.read().await.is_some(),
            "the gap in the event log is not silent"
        );
        assert_eq!(
            rt.last_seq.load(Ordering::SeqCst),
            0,
            "last_seq does not advance past a failed append"
        );
        assert!(!app.session_dir("abc").join("events.jsonl").exists(), "the line never landed");
        assert_eq!(
            app.session("abc").await.unwrap().status,
            SessionStatus::Running,
            "the in-memory state still advances"
        );
        drop(_guard);
        handle_agent_event(&app, "abc", &rt, r#"{"seq":2,"type":"status","state":"idle"}"#).await;
        assert_eq!(
            rt.last_seq.load(Ordering::SeqCst),
            2,
            "the next event succeeds and jumps past the lost one"
        );
        let events = std::fs::read_to_string(app.session_dir("abc").join("events.jsonl")).unwrap();
        assert_eq!(
            events, "{\"origin\":\"agent\",\"seq\":2,\"state\":\"idle\",\"type\":\"status\"}\n",
            "seq 1 stays lost"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_event_log_torn_inside_a_multibyte_character_keeps_the_last_good_seq() {
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        let mut events = Vec::new();
        events.extend_from_slice(b"{\"seq\":1,\"type\":\"status\",\"state\":\"working\"}\n");
        events.extend_from_slice(b"{\"seq\":2,\"type\":\"status\",\"state\":\"idle\"}\n");
        // A final line cut mid-write, inside the first byte of an em-dash — the ordinary debris of a crash.
        events.extend_from_slice(b"{\"seq\":3,\"type\":\"log\",\"message\":\"restarting \xE2");
        tokio::fs::write(app.session_dir("abc").join("events.jsonl"), &events)
            .await
            .unwrap();
        let rt = app.runtime("abc").await;
        assert_eq!(
            rt.last_seq.load(Ordering::SeqCst),
            2,
            "the torn line costs itself, not the whole file"
        );
        assert!(
            app.storage_alert.read().await.is_none(),
            "a torn line is a crash's debris, not a storage emergency"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_non_utf8_replay_line_costs_only_itself() {
        // The events_socket replay reads this way: byte-split, one tolerant line at a time.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\"seq\":1,\"type\":\"status\",\"state\":\"working\"}\n");
        // A line torn mid-write, inside the first byte of an em-dash, mid-file this time.
        bytes.extend_from_slice(b"{\"seq\":2,\"type\":\"log\",\"message\":\"restarting \xE2\"}\n");
        bytes.extend_from_slice(b"this line is not json\n");
        bytes.extend_from_slice(b"{\"seq\":3,\"type\":\"status\",\"state\":\"idle\"}\n");
        let replayed: Vec<u64> = bytes
            .split(|b| *b == b'\n')
            .filter(|chunk| !chunk.is_empty())
            .filter_map(|chunk| replay_line(chunk).map(|(seq, _)| seq))
            .collect();
        assert_eq!(
            replayed,
            vec![1, 3],
            "the corrupt and non-JSON lines are skipped; the replay reaches past them"
        );
    }

    #[tokio::test]
    async fn a_harness_log_torn_inside_a_multibyte_character_keeps_the_good_lines() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let mut logs = Vec::new();
        logs.extend_from_slice(b"{\"type\":\"harness_log\",\"level\":\"info\",\"message\":\"first\"}\n");
        logs.extend_from_slice(b"{\"type\":\"harness_log\",\"level\":\"info\",\"message\":\"second\"}\n");
        logs.extend_from_slice(b"{\"type\":\"harness_log\",\"level\":\"info\",\"message\":\"torn \xE2");
        tokio::fs::write(app.session_dir("abc").join("harness.jsonl"), &logs)
            .await
            .unwrap();
        let rt = app.runtime("abc").await;
        let logs = rt.logs.lock().await;
        assert_eq!(logs.len(), 2, "the torn line is dropped, the good lines stay in order");
        assert_eq!(logs[0]["message"], "first");
        assert_eq!(logs[1]["message"], "second");
        drop(logs);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_long_harness_log_keeps_exactly_the_last_max_logs_entries() {
        let (app, root) = app_with_colony("abc", SessionStatus::Idle).await;
        let mut logs = Vec::new();
        for i in 1..=250 {
            logs.extend_from_slice(
                format!("{{\"type\":\"harness_log\",\"level\":\"info\",\"message\":\"line {i}\"}}\n").as_bytes(),
            );
        }
        tokio::fs::write(app.session_dir("abc").join("harness.jsonl"), &logs)
            .await
            .unwrap();
        let rt = app.runtime("abc").await;
        let kept = rt.logs.lock().await;
        assert_eq!(kept.len(), MAX_LOGS, "the ring holds exactly {MAX_LOGS} well-formed entries");
        assert_eq!(
            kept[0]["message"], "line 51",
            "the first kept entry is the first of the last {MAX_LOGS}"
        );
        assert_eq!(kept.back().unwrap()["message"], "line 250", "the newest entry is last");
        drop(kept);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unreadable_event_log_alerts_on_first_use_and_admits_the_history_restarts() {
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        // The fault seam has no read op, but reading a directory is an error here (EISDIR), so an
        // unreadable file is staged as one — both files, to pin that both failures are carried.
        let dir = app.session_dir("abc");
        tokio::fs::create_dir(dir.join("events.jsonl")).await.unwrap();
        tokio::fs::create_dir(dir.join("harness.jsonl")).await.unwrap();
        let rt = app.runtime("abc").await;
        assert!(
            app.storage_alert.read().await.is_none(),
            "the failure waits for a caller that can report it, not the load itself"
        );
        app.report_load_error("abc", &rt).await;
        assert!(
            app.storage_alert.read().await.is_some(),
            "the read failure is recorded, not swallowed"
        );
        let logs = rt.logs.lock().await;
        let last = logs.back().unwrap();
        assert_eq!(last["type"], "harness_log");
        let message = last["message"].as_str().unwrap();
        assert!(message.contains("events.jsonl"), "{message}");
        assert!(message.contains("harness.jsonl"), "{message}");
        assert!(
            message.contains("may be recorded a second time"),
            "the events consequence is said plainly: {message}"
        );
        assert!(
            message.contains("the log history starts over"),
            "the log consequence is said plainly: {message}"
        );
        drop(logs);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn an_unreadable_event_log_names_only_its_own_consequence() {
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        // Reading a directory is an error here (EISDIR); harness.jsonl is left alone, so nothing
        // else can overwrite the alert and the report can be read back verbatim.
        tokio::fs::create_dir(app.session_dir("abc").join("events.jsonl"))
            .await
            .unwrap();
        let rt = app.runtime("abc").await;
        app.report_load_error("abc", &rt).await;
        let alert = app.storage_alert.read().await.clone().unwrap();
        assert!(
            alert.message.contains("read the colony's saved history"),
            "either file failing is a failure of the colony's saved history: {}",
            alert.message
        );
        let logs = rt.logs.lock().await;
        let message = logs.back().unwrap()["message"].as_str().unwrap();
        assert!(
            message.contains("the reconnect cursor restarts"),
            "the events consequence is named: {message}"
        );
        assert!(
            !message.contains("harness.jsonl") && !message.contains("log history starts over"),
            "a log that loaded fine is not accused of restarting: {message}"
        );
        drop(logs);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_question_still_open_on_disk_is_restored_on_load() {
        let dir = std::env::temp_dir().join(format!("colonizer-open-question-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let question = r#"{"seq":3,"ts":"2026-09-21T09:30:00.000Z","type":"question","question_id":"q1","questions":[{"header":"pin","options":[]}],"risk":"read_only"}"#;

        // Asked and never answered: the question is open, and its wait started when it was asked.
        std::fs::write(
            dir.join("events.jsonl"),
            format!("{{\"seq\":1,\"type\":\"status\",\"state\":\"working\"}}\n{question}\n"),
        )
        .unwrap();
        let rt = Runtime::load(&dir);
        let (id, questions, risk) = rt
            .open_question
            .try_lock()
            .unwrap()
            .clone()
            .expect("the question is still open");
        assert_eq!(id, "q1");
        assert_eq!(questions, vec![json!({"header": "pin", "options": []})]);
        assert_eq!(
            risk,
            QuestionRisk::ReadOnly,
            "the risk class rides out the restart with the question"
        );
        assert_eq!(
            rt.activity.try_lock().unwrap().question_since,
            Some("2026-09-21T09:30:00Z".parse::<DateTime<Utc>>().unwrap())
        );
        assert_eq!(rt.agent_seq.load(Ordering::SeqCst), 3, "the cursor pass is unchanged");

        // Asked and then answered: nothing is open.
        std::fs::write(
            dir.join("events.jsonl"),
            format!("{question}\n{{\"seq\":4,\"type\":\"question_answered\",\"question_id\":\"q1\",\"answers\":{{}}}}\n"),
        )
        .unwrap();
        let rt = Runtime::load(&dir);
        assert!(rt.open_question.try_lock().unwrap().is_none());
        assert!(rt.activity.try_lock().unwrap().question_since.is_none());
        assert_eq!(rt.agent_seq.load(Ordering::SeqCst), 4);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The risk class folds on replay exactly as the live path folds it (`events.rs`): a question
    /// from an older runner, with no `risk` on disk, restarts as a workspace write; a class a
    /// future runner knows stays above every ceiling, and the judge must keep refusing it.
    #[test]
    fn a_restored_question_risk_folds_the_same_way_the_live_path_does() {
        let dir = std::env::temp_dir().join(format!("colonizer-open-question-{}", short_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stored = |risk: &str| format!(r#"{{"seq":1,"type":"question","question_id":"q1","questions":[]{risk}}}"#);
        for (line, expected, why) in [
            (stored(""), QuestionRisk::WorkspaceWrite, "an older runner left the field out"),
            (stored(r#","risk":null"#), QuestionRisk::WorkspaceWrite, "null is absent"),
            (
                stored(r#","risk":"unknown_string_from_a_newer_runner""#),
                QuestionRisk::Unknown,
                "a string outside the vocabulary",
            ),
            (stored(r#","risk":3"#), QuestionRisk::Unknown, "not a string at all"),
            (
                stored(r#","risk":{"note":"trust me"}"#),
                QuestionRisk::Unknown,
                "not a string at all",
            ),
        ] {
            std::fs::write(dir.join("events.jsonl"), format!("{line}\n")).unwrap();
            let rt = Runtime::load(&dir);
            let (_, _, risk) = rt.open_question.try_lock().unwrap().clone().expect("still open");
            assert_eq!(risk, expected, "{why}: for {line}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_fresh_colony_with_no_stored_events_loads_quietly_at_seq_zero() {
        let (app, root) = app_with_colony("abc", SessionStatus::Starting).await;
        let rt = app.runtime("abc").await;
        app.report_load_error("abc", &rt).await;
        assert_eq!(rt.last_seq.load(Ordering::SeqCst), 0);
        assert!(rt.logs.lock().await.is_empty());
        assert!(
            app.storage_alert.read().await.is_none(),
            "a colony that has never emitted an event has no events.jsonl, and that is not a failure"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // -- org workspaces on and off --------------------------------------------------------------
}
