//! Findings that arrived with nothing to judge them, and the sweep that files them when a model
//! comes back.
//!
//! A finding is judged host-side before it is filed (validation.rs), and that judgement needs a
//! model the host can route: the judge's own, a background or subagent model naming a provider, or
//! the agent module's model through an Anthropic provider (autonomy.rs `host_model`). On an install
//! with none of those — subscription-only, say (issue #143) — every finding used to be logged once
//! and dropped. Now it waits in its colony's queue (`out/findings-queue.jsonl`, the directory the
//! microVM mounts at `/harness/out`) and a sweep retries the queue every minute against whatever
//! model is callable by then (issue #1154). One install-level card in the attention feed says
//! findings are waiting, so the state is visible without a warn line per finding.

use crate::{Shared, sessions::Session};
use chrono::Utc;
use serde_json::{Value, json};
use std::time::Duration;

/// The file a colony's unfiled findings wait in, under its `out/` directory — the directory the
/// microVM mounts at `/harness/out`, so a queued finding sits beside the colony's own output.
pub(crate) const QUEUE_FILE: &str = "findings-queue.jsonl";

/// The reason every queued ledger line carries, in full, because it is the whole explanation the
/// Inspector shows for a finding that has not been filed.
pub(crate) const QUEUED_REASON: &str = "no model the host can call for judgement: add a model provider in Settings → Model providers, or set judge_model; the finding is queued and files when one is callable";

/// How often the sweep retries the queues.
const TICK: Duration = Duration::from_secs(60);

/// Parks a finding the host cannot judge right now: one line in the colony's queue carrying the
/// original event, and one `queued` line on the findings ledger the Inspector reads. No session-log
/// line — the install-level card replaces a warn per finding (issue #1154).
pub(crate) async fn queue(app: &Shared, id: &str, event: &Value, finding: &crate::findings::Finding) {
    let dir = app.session_dir(id).join("out");
    let path = dir.join(QUEUE_FILE);
    let line = json!({"id": crate::util::short_id(), "queued_at": Utc::now().to_rfc3339(), "event": event}).to_string();
    // The queue lives under `out/`, which exists only once something creates it — the mounted
    // directory appears at boot, and a queued finding must not depend on that.
    let appended = match tokio::fs::create_dir_all(&dir).await {
        Ok(()) => crate::util::append_line(&path, &line).await,
        Err(e) => Err(anyhow::Error::from(e)),
    };
    if let Err(e) = appended {
        // The ledger line below is the colony's record either way; what failed is the durable copy
        // the sweep would retry from, and the gap is reported rather than passed silently.
        app.storage_failed("append to the colony's findings queue", &e).await;
    }
    crate::validation::record(
        app,
        id,
        &json!({"title": finding.title, "state": "queued", "reason": QUEUED_REASON}),
    )
    .await;
}

/// The colonies with findings waiting in their queue, and how many each waits with. Colonies with
/// none are left out, and the list is sorted by id, so the card's rows never shuffle between polls.
async fn queued_counts(app: &Shared) -> Vec<(Session, usize)> {
    let sessions = app.sessions.read().await.clone();
    let mut out: Vec<(Session, usize)> = sessions
        .into_iter()
        .map(|s| {
            let waiting = std::fs::read_to_string(app.session_dir(&s.id).join("out").join(QUEUE_FILE))
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            (s, waiting)
        })
        .filter(|(_, waiting)| *waiting > 0)
        .collect();
    out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    out
}

/// The install-level card while findings are waiting with nothing to judge them: one card however
/// many colonies wait, so the attention feed carries the state without a finding each. Nothing is
/// shown while findings are switched off (nothing new queues, and a stale queue drains as
/// "ignored"), and nothing while a model is callable — the sweep is draining the queues then.
pub(crate) async fn cards(app: &Shared) -> Vec<Value> {
    let modules = app.modules.read().await.clone();
    if !crate::sessions::findings_enabled(app, &modules) {
        return Vec::new();
    }
    let queued = queued_counts(app).await;
    if queued.is_empty() {
        return Vec::new();
    }
    if crate::autonomy::host_model(app).await.is_some() {
        return Vec::new();
    }
    let count: usize = queued.iter().map(|(_, n)| n).sum();
    let colonies = queued
        .iter()
        .map(|(s, n)| {
            json!({
                "id": s.id,
                "repo": s.repo,
                "org": s.org,
                "issue": s.issue,
                "issue_title": s.issue_title,
                "count": n,
            })
        })
        .collect::<Vec<_>>();
    vec![json!({
        "title": "Findings and judge are off",
        "detail": "add a model provider or set judge_model — queued findings file once a model is callable",
        "count": count,
        "colonies": colonies,
    })]
}

/// One pass over every colony's queue, with a model to file by. Each queued event runs the ordinary
/// [`crate::events::file_finding`] pipeline — validation, ledger, GitHub — and the file is then
/// rewritten with what is left: a line whose event re-queued itself (the model vanished mid-sweep)
/// survives under its fresh id, and a line that no longer parses or names no id is dropped, so a
/// corrupt line cannot wedge the queue forever.
pub(crate) async fn tick_once(app: &Shared) {
    if crate::autonomy::host_model(app).await.is_none() {
        return;
    }
    for (session, _) in queued_counts(app).await {
        let path = app.session_dir(&session.id).join("out").join(QUEUE_FILE);
        let Ok(before) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut processed: Vec<String> = Vec::new();
        for line in before.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(id) = entry["id"].as_str().map(str::to_string) else {
                continue;
            };
            processed.push(id);
            let Some(event) = entry.get("event").cloned() else {
                continue;
            };
            let rt = app.runtime(&session.id).await;
            crate::events::file_finding(app.clone(), session.id.clone(), rt, event).await;
        }
        // The re-read is the race guard: a line stays only when it still parses, carries an id, and
        // this pass did not process it — the fresh id a mid-sweep re-queue wrote survives, the rest
        // goes with the pass.
        let after = std::fs::read_to_string(&path).unwrap_or_default();
        let kept: Vec<&str> = after
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| {
                serde_json::from_str::<Value>(l)
                    .ok()
                    .and_then(|e| e["id"].as_str().map(str::to_string))
                    .is_some_and(|id| !processed.contains(&id))
            })
            .collect();
        let outcome = if kept.is_empty() {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                // Nothing left to drain is the good case, whoever removed the file first.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(anyhow::Error::from(e)),
            }
        } else {
            let mut out = kept.join("\n");
            out.push('\n');
            crate::util::write_atomic(&path, out.as_bytes()).await
        };
        if let Err(e) = outcome {
            // What failed is the drain, not the findings — the next tick sees the same file and
            // tries again — but the fault is named rather than passed silently.
            app.storage_failed("rewrite of the colony's findings queue", &e).await;
        }
    }
}

/// This module's background work, started once by `server::start_tasks`: the retry sweep.
pub(crate) fn start_tasks(app: &crate::Shared) {
    tokio::spawn(run(app.clone()));
}

/// Every minute, and once at once: the interval's first tick fires immediately, so queues a restart
/// left behind drain as soon as a model is callable.
async fn run(app: Shared) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        tick_once(&app).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionStatus;
    use crate::tests::test_app;

    const HUNTER: &str = "hunter";

    /// One colony to file for, as the Inspector lists it.
    fn colony() -> crate::sessions::Session {
        let mut s = crate::sessions::tests::colony("acme", SessionStatus::Running);
        s.id = HUNTER.into();
        s
    }

    /// An install over `root` with the agent module's settings set as given, one colony in the
    /// list, and its session directory in place.
    async fn install(root: &std::path::Path, agent: &[(&str, Value)]) -> Shared {
        let app = test_app(root);
        {
            let mut modules = app.modules.write().await;
            for (key, value) in agent {
                modules.agent.settings.insert((*key).into(), value.clone());
            }
        }
        app.sessions.write().await.push(colony());
        tokio::fs::create_dir_all(app.session_dir(HUNTER)).await.unwrap();
        app
    }

    /// Writes the model providers on disk, as an operator's `providers.json` would carry them.
    fn write_providers(root: &std::path::Path, base_url: &str) {
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(
            root.join("config/providers.json"),
            serde_json::to_vec(&[json!({"id": "stub", "name": "Stub", "base_url": base_url, "auth": "none"})]).unwrap(),
        )
        .unwrap();
    }

    /// A stub model provider: every request answered with `decision` as the model's text, so a
    /// validation call makes a real host-side call and gets a deterministic verdict.
    async fn stub_model(decision: &str) -> String {
        let body = json!({"content": [{"type": "text", "text": decision}]}).to_string();
        let router = axum::Router::new().fallback(move |_b: axum::body::Bytes| {
            let body = body.clone();
            async move {
                axum::response::Response::builder()
                    .status(200)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    /// One finding event, the shape `findings::parse` reads out of a colony's event stream.
    fn finding_event() -> Value {
        json!({
            "type": "finding",
            "title": "career pages are promised",
            "body": "llms.txt says the pages exist",
            "evidence": "read model.rs",
        })
    }

    /// Issue #1154's happy path on a subscription-only install: the agent module's plain `model`
    /// cannot be called host-side (no Anthropic provider — issue #143), but a provider-qualified
    /// `background_model` can, so the finding is judged through it and nothing is queued. Filing
    /// onward hits GitHub and fails in tests, so the assertion stops at the validated ledger line.
    #[tokio::test]
    async fn a_provider_qualified_background_model_judges_without_queueing() {
        let root = crate::tests::temp_root();
        let url = stub_model(r#"{"real": true, "severity": "high", "reason": "the login query is built from raw input"}"#).await;
        write_providers(&root, &url);
        let app = install(
            &root,
            &[
                ("model", json!("claude-opus-5-5")),
                ("background_model", json!("stub/elsewhere")),
            ],
        )
        .await;
        let rt = app.runtime(HUNTER).await;
        crate::events::file_finding(app.clone(), HUNTER.into(), rt, finding_event()).await;
        let ledger = std::fs::read_to_string(app.session_dir(HUNTER).join("findings.jsonl")).unwrap();
        assert!(ledger.contains("\"state\":\"validated\""), "{ledger}");
        assert!(
            !app.session_dir(HUNTER).join("out").join(QUEUE_FILE).exists(),
            "a finding a callable model judged is never queued"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The bug itself: with no model the host can call, the finding is queued with its original
    /// event and minuted as queued, and one card covers the install however many findings wait.
    #[tokio::test]
    async fn with_no_callable_model_a_finding_queues_and_the_card_rises() {
        let root = crate::tests::temp_root();
        let app = install(&root, &[("model", json!("claude-opus-5-5"))]).await;
        let queue = app.session_dir(HUNTER).join("out").join(QUEUE_FILE);
        let rt = app.runtime(HUNTER).await;
        crate::events::file_finding(app.clone(), HUNTER.into(), rt, finding_event()).await;
        let stored = std::fs::read_to_string(&queue).unwrap();
        let line: Value = serde_json::from_str(stored.lines().next().unwrap()).unwrap();
        assert_eq!(
            line["event"]["title"], "career pages are promised",
            "the original event waits: {line}"
        );
        let ledger = std::fs::read_to_string(app.session_dir(HUNTER).join("findings.jsonl")).unwrap();
        assert!(ledger.contains("\"state\":\"queued\""), "{ledger}");
        let raised = cards(&app).await;
        assert_eq!(raised.len(), 1, "one card however many findings wait");
        assert_eq!(raised[0]["count"], 1);
        assert_eq!(raised[0]["colonies"][0]["id"], HUNTER);
        assert_eq!(raised[0]["colonies"][0]["org"], "acme");

        // A second finding waits beside the first; still the one card.
        let rt = app.runtime(HUNTER).await;
        crate::events::file_finding(app.clone(), HUNTER.into(), rt, finding_event()).await;
        let raised = cards(&app).await;
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0]["count"], 2);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The queue drains through the whole pipeline once a model is callable, and the sweep that has
    /// no model to call leaves the file exactly as it found it.
    #[tokio::test]
    async fn the_sweep_drains_the_queue_once_a_model_is_callable() {
        let root = crate::tests::temp_root();
        let app = install(&root, &[("model", json!("claude-opus-5-5"))]).await;
        let queue = app.session_dir(HUNTER).join("out").join(QUEUE_FILE);
        let rt = app.runtime(HUNTER).await;
        crate::events::file_finding(app.clone(), HUNTER.into(), rt, finding_event()).await;
        let before = std::fs::read_to_string(&queue).unwrap();

        // Still nothing callable: the sweep leaves the file untouched.
        tick_once(&app).await;
        assert_eq!(std::fs::read_to_string(&queue).unwrap(), before, "no model, no sweep");

        // A provider arrives and a background model names it; the stub rejects the finding, which
        // still proves the queued event ran the whole pipeline.
        let url = stub_model(r#"{"real": false, "reason": "not a real problem"}"#).await;
        write_providers(&root, &url);
        app.modules
            .write()
            .await
            .agent
            .settings
            .insert("background_model".into(), json!("stub/elsewhere"));
        tick_once(&app).await;
        let ledger = std::fs::read_to_string(app.session_dir(HUNTER).join("findings.jsonl")).unwrap();
        assert!(ledger.contains("\"state\":\"rejected\""), "{ledger}");
        assert!(!queue.exists(), "a drained queue leaves no file behind");
        assert!(cards(&app).await.is_empty(), "nothing waiting, no card");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Lines the queue cannot use — a corrupt line, one with no id — are dropped by the sweep
    /// instead of wedging it, while a line nothing processed yet stays put.
    #[tokio::test]
    async fn the_sweep_drops_lines_it_cannot_use_and_keeps_the_rest() {
        let root = crate::tests::temp_root();
        let app = install(&root, &[("model", json!("claude-opus-5-5"))]).await;
        let queue = app.session_dir(HUNTER).join("out").join(QUEUE_FILE);
        let rt = app.runtime(HUNTER).await;
        crate::events::file_finding(app.clone(), HUNTER.into(), rt, finding_event()).await;
        let good = std::fs::read_to_string(&queue).unwrap().trim_end().to_string();
        std::fs::write(&queue, format!("{good}\nnot json at all\n{{\"queued_at\": \"no id\"}}\n")).unwrap();

        // Still no model: even the corrupt lines wait for a sweep that can file something.
        tick_once(&app).await;
        assert!(
            std::fs::read_to_string(&queue).unwrap().contains("not json"),
            "no model, no sweep"
        );

        // With a model, the good line is processed away and the unusable lines are dropped with it.
        let url = stub_model(r#"{"real": false, "reason": "not a real problem"}"#).await;
        write_providers(&root, &url);
        app.modules
            .write()
            .await
            .agent
            .settings
            .insert("background_model".into(), json!("stub/elsewhere"));
        tick_once(&app).await;
        assert!(!queue.exists(), "nothing usable was left, so the file is gone");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Findings switched off: nothing queues into this state, so no card asks for attention that is
    /// switched off — a stale queue drains as "ignored" once a model is back.
    #[tokio::test]
    async fn no_card_while_filing_findings_is_switched_off() {
        let root = crate::tests::temp_root();
        let app = install(&root, &[("model", json!("claude-opus-5-5"))]).await;
        let queue = app.session_dir(HUNTER).join("out").join(QUEUE_FILE);
        let rt = app.runtime(HUNTER).await;
        crate::events::file_finding(app.clone(), HUNTER.into(), rt, finding_event()).await;
        assert!(queue.exists(), "the finding queued while filing was still on");
        let mut modules = app.modules.write().await;
        modules.publish.settings.insert("file_findings".into(), json!(false));
        drop(modules);
        assert!(cards(&app).await.is_empty(), "switched off, no card");
        assert!(queue.exists(), "the card's absence does not drain anything by itself");
        let _ = std::fs::remove_dir_all(root);
    }
}
