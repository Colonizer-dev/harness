//! `GET /api/status`: the full body for the cockpit, and the reduced allowlist body for callers
//! without the API token (fleet peers).

use crate::app::PROBE_LIMIT;
use crate::config::{ModulesConfig, setting_u64};
use crate::util::exec_within;
use crate::{
    Shared, StorageAlert, StorageAlertKind, auth, claude_login, config, diagnosis, gateway, github, mesh, modules, orgs,
    providers, reclaim, resolve_guest_claude_bin, runtime, sessions,
};
use anyhow::{Result, anyhow};
use axum::{
    Json,
    extract::{Extension, Query, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{future::Future, path::Path as FsPath, time::Duration};
use tokio::process::Command;

/// The `storage` key of `/api/status`: `ok` while every write was confirmed, else the sticky alert
/// (`ts` and `recovered_at` in the same RFC 3339 form the `harness_log` frames use). A recovered
/// alert is `ok` again but keeps its message, time and count: the gap it reports still happened.
/// `ok` says whether writes are going through, so load damage is `ok` with a null `recovered_at`:
/// its `kind` is what keeps it on screen, since the colonies it reports never come back.
/// Every payload also carries the last queue-tick free-space verdict: `free_bytes` (null before the
/// first reading or when `df` fails), `warn_free_bytes` and `min_free_bytes` (0 = off), `low_disk`
/// (below the higher of the two), and `admission_paused` (below the floor, so the queue holds).
fn storage_status(alert: Option<StorageAlert>, verdict: &reclaim::FreeSpaceVerdict) -> Value {
    let mut value = match alert {
        None => json!({"ok": true}),
        Some(alert) => json!({
            "ok": alert.kind == StorageAlertKind::LoadDamage || alert.recovered_at.is_some(),
            "kind": alert.kind,
            "message": alert.message,
            "ts": alert.ts,
            "failures": alert.failures,
            "recovered_at": alert.recovered_at,
        }),
    };
    value["free_bytes"] = json!(verdict.free_bytes);
    value["warn_free_bytes"] = json!(verdict.warn_free_bytes);
    value["min_free_bytes"] = json!(verdict.min_free_bytes);
    value["low_disk"] = json!(verdict.low_disk);
    value["admission_paused"] = json!(verdict.admission_paused);
    value
}

/// An overall bound on the mesh branch of `/api/status`, not just its individual subprocesses:
/// `Mesh::status` waits on `Mesh`'s internal `running` lock, which `ensure_started` can hold across
/// calls that are themselves unbounded, so without this a wedged `headscale` during a boot parks
/// the status poll forever behind a mutex.
const MESH_STATUS_LIMIT: Duration = Duration::from_secs(15);

/// The `mesh` key of `/api/status`. `live` is awaited only when the mesh is enabled and its
/// binaries are vendored: the running mesh's own status, or the reason none could be built.
async fn mesh_status(modules: &ModulesConfig, assets: Option<&FsPath>, live: impl Future<Output = Result<Value>>) -> Value {
    if !modules.mesh_enabled() {
        json!({"enabled": false, "provider": "none"})
    } else if !assets.is_some_and(mesh::binaries_present) {
        // Not an error the operator can clear: this platform has no mesh binaries to
        // vendor. It goes in `detail`, not `error`: anything in `error` is read as a
        // fault, and this one used to paint every Mac's runtime red.
        json!({"enabled": true, "provider": "headscale", "state": "unavailable",
               "detail": "colonies use a loopback port on this platform", "error": Value::Null})
    } else {
        let live = tokio::time::timeout(MESH_STATUS_LIMIT, live)
            .await
            .unwrap_or_else(|_| Err(anyhow!("mesh status timed out after {MESH_STATUS_LIMIT:?}")));
        match live {
            Ok(status) => status,
            Err(e) => json!({"enabled": true, "provider": "headscale", "state": "error", "error": format!("{e:#}")}),
        }
    }
}

/// The query parameters of `GET /api/status`. `fresh=1` skips the runtime cache and re-probes, so
/// the UI's "Check again" button gets a real answer instead of a cached one; any value but `0` or
/// `false` counts, so a bare `?fresh` works too.
#[derive(Deserialize)]
pub struct StatusQuery {
    fresh: Option<String>,
}

/// The `GET /api/status` body for callers without the API token: an allowlist — version, counts,
/// capacity figures and health only — built from scratch, never the full body with fields removed.
/// Fleet peers poll this endpoint without a token, so `fleet::summary_from_status_json` reads
/// exactly these keys and defaults the rest. NEVER add identity here: no repo or issue names, no
/// org names, no paths or URLs, no hostnames or host ids, no GitHub or Claude account details.
async fn reduced_status(app: &Shared) -> Value {
    let modules = app.modules.read().await.clone();
    let sessions = app.sessions.read().await;
    let queue_depth = sessions
        .iter()
        .filter(|s| s.status == sessions::SessionStatus::Queued)
        .count();
    // The same "holds a slot" definition the full body counts.
    let microvms_live = sessions.iter().filter(|s| s.holds_slot()).count();
    drop(sessions);
    let microvms_ceiling = orgs::global_max_parallel(&modules);
    // Cached probes only: strangers must not force the probe subprocesses to rerun.
    let (runtime, host) = tokio::join!(runtime::status_runtime(app, false), runtime::status_host(app, false),);
    let mut host_value = json!({
        "microvms_live": microvms_live,
        "microvms_ceiling": microvms_ceiling,
    });
    // Numeric capacity figures only: the id, the hostname and the probe timestamp stay in the full
    // body, and a measurement that failed is omitted rather than faked as zero.
    for (key, value) in [
        ("cpu_cores", host.cpu_cores.map(|n| json!(n))),
        ("memory_total_bytes", host.memory_total_bytes.map(|n| json!(n))),
        ("memory_used_bytes", host.memory_used_bytes.map(|n| json!(n))),
        ("load", host.load.map(|load| json!(load))),
        ("disk_total_bytes", host.disk_total_bytes.map(|n| json!(n))),
        ("disk_used_bytes", host.disk_used_bytes.map(|n| json!(n))),
        ("disk_free_bytes", host.disk_free_bytes.map(|n| json!(n))),
    ] {
        if let Some(value) = value {
            host_value[key] = value;
        }
    }
    // The storage message names files, so only its verdict crosses over.
    let storage_ok = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default())
        .get("ok")
        .cloned()
        .unwrap_or(json!(true));
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "queue_depth": queue_depth,
        "host": host_value,
        "runtime": {
            "platform": runtime.platform,
            "os": {
                "vendor": runtime.os.vendor,
                "name": runtime.os.name,
                "version": runtime.os.version,
            },
        },
        "storage": {"ok": storage_ok},
    })
}

pub(crate) async fn status(
    State(app): State<Shared>,
    Extension(authenticated): Extension<auth::Authenticated>,
    Query(query): Query<StatusQuery>,
) -> Json<Value> {
    // No token: the allowlist body for fleet peers and other strangers, built fresh below — never
    // the full body with fields removed.
    if !authenticated.0 {
        return Json(reduced_status(&app).await);
    }
    let mut msb = Command::new(&app.cfg.msb);
    msb.arg("--version");
    let cred = app.claude_cred();
    let fresh = query.fresh.as_deref().is_some_and(|v| !matches!(v, "0" | "false"));
    let (user, msb_version, claude_bin, claude, runtime, host) = tokio::join!(
        github::viewer(&app),
        exec_within(PROBE_LIMIT, &mut msb),
        resolve_guest_claude_bin(&app),
        claude_login::claude_status(&app, cred.as_ref()),
        runtime::status_runtime(&app, fresh),
        runtime::status_host(&app, fresh),
    );
    let modules = app.modules.read().await.clone();
    // The same "holds a slot" definition `queue::has_room` counts against the parallel limit — a live colony
    // or a live-origin publish keeps its microVM claimed, while a publish from a stopped colony holds
    // nothing. Kept in step with it; `Session::holds_slot` is the shared predicate both sides express.
    let microvms_live = app.sessions.read().await.iter().filter(|s| s.holds_slot()).count();
    let microvms_ceiling = orgs::global_max_parallel(&modules);
    // Queued colonies hold no microVM (see `Session::holds_slot`), so this is disjoint from
    // `microvms_live`. Carried in `/api/status` so a fleet peer-poll of this endpoint (issue #231)
    // gets everything `fleet::HostSummary` needs without a second round trip.
    let queue_depth = app
        .sessions
        .read()
        .await
        .iter()
        .filter(|s| s.status == sessions::SessionStatus::Queued)
        .count();
    // Cheap, no filesystem I/O: both predicates read in-memory session fields only.
    let (reclaimable, unpushed) = {
        let cfg = reclaim::ReclaimConfig::from_modules(&modules);
        let now = chrono::Utc::now();
        let sessions = app.sessions.read().await;
        (
            sessions
                .iter()
                .filter(|s| reclaim::reclaim_due(s, now, cfg.retention_secs))
                .count(),
            sessions.iter().filter(|s| reclaim::unpushed_work(s)).count(),
        )
    };
    // Computed before the `json!` literal below, which moves `runtime` into the payload: the host
    // object borrows its kvm answer, so the borrow must end before the move.
    let host_value = runtime::host_json(&host, runtime.kvm.as_ref(), microvms_live, microvms_ceiling);
    let live = async {
        match app.mesh().await {
            Ok(mesh) => Ok(mesh.status().await),
            Err(e) => Err(e),
        }
    };
    let mesh = mesh_status(&modules, app.cfg.assets.as_deref(), live).await;
    let sandbox_schema = modules::schema_for("sandbox", &modules.sandbox.provider, &app.agents);
    let asset = |rel: &str| app.cfg.assets.as_ref().is_some_and(|a| a.join(rel).exists());
    let storage_alert = app.shown_storage_alert().await;
    // The last queue-tick verdict, read with no I/O: the queue refreshes it every tick.
    let disk_verdict = app.disk_verdict.lock().await.clone();
    // One entry per configured model provider, so a provider the colony fan-out is degrading is visible
    // from the status poll without opening the providers screen. Named `model_providers` because the
    // `modules` section's `sandbox`/`source`/`mesh` entries are this status's other "providers".
    let model_providers: Vec<Value> = app
        .providers()
        .iter()
        .map(|p| {
            let usage = app.gateway.usage(&p.id);
            let health = gateway::health(&usage);
            json!({
                "id": p.id,
                "name": p.name,
                "requests": usage.requests,
                "failure_pct": health.failure_pct,
                "avg_latency_ms": health.avg_latency_ms,
                "degraded": health.degraded,
            })
        })
        .collect();
    // Whether every routable provider's plan is out, and the earliest reset: the queue holder's
    // own words, so the overview banner and the queue gate never disagree.
    let quota = providers::quota_status(&app).await;
    // The host-wide stall (§diagnosis): live colonies, a waiting queue, and no colony producing
    // an event for ten minutes. Cheap — runtime stamps, else file mtimes, never file contents.
    let stall = diagnosis::status_stall(&app).await;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "queue_depth": queue_depth,
        "stall": stall,
        "reclaim": {"reclaimable": reclaimable, "unpushed": unpushed},
        "github": match user {
            Ok(u) => json!({"connected": true, "login": u["login"], "name": u["name"], "avatar_url": u["avatar_url"], "source": github::token_source(&app)}),
            Err(e) => json!({"connected": false, "error": format!("{e:#}")}),
        },
        "claude": claude,
        "sandbox": {
            "provider": modules.sandbox.provider,
            "image": config::setting_str(&modules.sandbox, &sandbox_schema, "image"),
            "cpus": setting_u64(&modules.sandbox, &sandbox_schema, "cpus"),
            "memory": config::setting_str(&modules.sandbox, &sandbox_schema, "memory"),
            "max_parallel": setting_u64(&modules.sandbox, &sandbox_schema, "max_parallel"),
            "msb_version": msb_version.ok().map(|v| v.trim().to_string()),
            "claude_bin": claude_bin.as_ref().ok().map(|p| p.display().to_string()),
            "claude_bin_error": claude_bin.err().map(|e| format!("{e:#}")),
        },
        "mesh": mesh,
        "storage": storage_status(storage_alert, &disk_verdict),
        "runtime": runtime,
        "host": host_value,
        "model_providers": model_providers,
        "quota": json!({
            "paused": quota.paused,
            "reason": quota.reason,
            "reset_at": quota.reset_at,
            "reset_unix": quota.reset_unix,
            "providers": quota.providers,
            "kind": quota.kind,
        }),
        // The anti-spam ledger's tallies and limits (issue #311): counts by class, never colony ids.
        "ledger": app.ledger.snapshot(),
        "modules": {
            "source": modules.source.provider,
            "sandbox": modules.sandbox.provider,
            "mesh": if modules.mesh.enabled { modules.mesh.provider.as_str() } else { "none" },
            "agent": modules.agent.provider,
            "publish": modules.publish.provider,
            // Off, or never configured, reads as "none" like the mesh's loopback provider does.
            "notify": modules
                .notify
                .as_ref()
                .filter(|c| c.enabled)
                .map(|c| c.provider.as_str())
                .unwrap_or("none"),
        },
        "assets": {
            "path": app.cfg.assets.as_ref().map(|p| p.display().to_string()),
            "agentd": asset("bin/colonizer-agentd"),
            "headscale": asset("vendor/headscale"),
            "tailscale": asset("vendor/tailscale/tailscaled"),
            "web": asset("web/index.html"),
            "agents": app.agents.iter().map(|a| &a.id).collect::<Vec<_>>(),
        },
    }))
}

/// The API routes this module serves. `server::api_routes` merges them into the cockpit's router,
/// behind the activity log's route layer and `host_guard`.
pub(crate) fn routes() -> axum::Router<crate::Shared> {
    use axum::routing;
    axum::Router::new().route("/api/status", routing::get(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::load_sessions;
    use crate::tests::{temp_root, test_app};
    use chrono::Utc;
    use std::sync::Arc;

    #[test]
    fn the_status_storage_key_is_ok_until_a_write_goes_unconfirmed() {
        let verdict = reclaim::FreeSpaceVerdict::default();
        let value = storage_status(None, &verdict);
        assert_eq!(value["ok"], true);
        assert!(value["free_bytes"].is_null(), "no queue tick has measured yet: {value}");
        assert_eq!(value["warn_free_bytes"], json!(verdict.warn_free_bytes));
        assert_eq!(value["min_free_bytes"], json!(verdict.min_free_bytes));
        assert_eq!(value["low_disk"], false);
        assert_eq!(value["admission_paused"], false);
        let alert = StorageAlert {
            kind: StorageAlertKind::Write,
            message: "save the session list failed: disk is full".into(),
            ts: Utc::now(),
            failures: 3,
            recovered_at: None,
        };
        let value = storage_status(Some(alert), &verdict);
        assert_eq!(value["ok"], false);
        assert_eq!(value["kind"], "write");
        assert_eq!(value["message"], "save the session list failed: disk is full");
        assert_eq!(value["failures"], 3);
        assert!(
            value["ts"].is_string(),
            "the ts is the RFC 3339 string the harness_log frames use: {value}"
        );
        assert!(value["recovered_at"].is_null(), "still failing: {value}");
    }

    /// The sequence from #220's disk-full incident: writes fail, space is freed and they succeed,
    /// then the disk fills again. Each step must read differently, and the count never resets.
    #[tokio::test]
    async fn a_storage_alert_reports_recovery_and_a_later_failure_undoes_it() {
        let root = temp_root();
        let app = test_app(&root);
        app.storage_succeeded().await;
        assert!(
            app.storage_alert.read().await.is_none(),
            "a success with no failure behind it raises nothing"
        );

        app.storage_failed("save the session list", &anyhow!("disk is full")).await;
        let failing = storage_status(app.storage_alert.read().await.clone(), &reclaim::FreeSpaceVerdict::default());
        assert_eq!(failing["ok"], false);
        assert!(failing["recovered_at"].is_null(), "{failing}");
        assert_eq!(failing["failures"], 1, "{failing}");
        assert!(failing["message"].as_str().unwrap().contains("disk is full"), "{failing}");
        assert!(failing["ts"].is_string(), "{failing}");

        app.persist_sessions().await.unwrap();
        let recovered = storage_status(app.storage_alert.read().await.clone(), &reclaim::FreeSpaceVerdict::default());
        assert_eq!(recovered["ok"], true, "a write went through, so the disk is not broken now");
        assert!(recovered["recovered_at"].is_string(), "{recovered}");
        assert_eq!(
            recovered["ts"], failing["ts"],
            "the failure it recovered from is still the one shown"
        );
        assert_eq!(recovered["failures"], 1);
        assert!(recovered["message"].as_str().unwrap().contains("disk is full"));
        app.storage_succeeded().await;
        let again = storage_status(app.storage_alert.read().await.clone(), &reclaim::FreeSpaceVerdict::default());
        assert_eq!(
            again["recovered_at"], recovered["recovered_at"],
            "recovery is stamped by the first success, not moved by every later one"
        );

        app.storage_failed("save the session list", &anyhow!("disk is full again"))
            .await;
        let failing_again = storage_status(app.storage_alert.read().await.clone(), &reclaim::FreeSpaceVerdict::default());
        assert_eq!(failing_again["ok"], false, "a new failure makes the alert current again");
        assert!(failing_again["recovered_at"].is_null(), "{failing_again}");
        assert_eq!(failing_again["failures"], 2, "failures stay cumulative across a recovery");
        assert_ne!(
            failing_again["ts"], recovered["ts"],
            "the new failure is stamped afresh: {failing_again}"
        );
        assert!(
            failing_again["message"].as_str().unwrap().contains("disk is full again"),
            "{failing_again}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Load damage reports colonies no save brings back (#371): the saves after startup must not mark
    /// it recovered, and a write failure on top of it is shown while it lasts but must not overwrite
    /// it, so once writes go through again the load damage is shown as it was.
    #[tokio::test]
    async fn load_damage_is_never_recovered_and_outlasts_a_write_failure() {
        let root = temp_root();
        let path = root.join("data/sessions.json");
        std::fs::write(&path, b"this is not json").unwrap();
        let (_, damage) = load_sessions(&path).unwrap();
        let mut app = test_app(&root);
        Arc::get_mut(&mut app).unwrap().load_damage = damage;

        app.persist_sessions().await.unwrap();
        let damaged = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(damaged["kind"], "load_damage");
        assert_eq!(damaged["ok"], true, "writes are going through; the kind keeps it shown");
        assert!(
            damaged["recovered_at"].is_null(),
            "a save does not bring the colonies back: {damaged}"
        );
        assert!(damaged["message"].as_str().unwrap().contains(".corrupt-"), "{damaged}");

        app.storage_failed("save the session list", &anyhow!("disk is full")).await;
        let failing = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(failing["kind"], "write");
        assert_eq!(failing["ok"], false);
        assert_eq!(failing["failures"], 1, "the load damage is not counted as a failed write");

        app.persist_sessions().await.unwrap();
        let after = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(after, damaged, "the load damage is shown again, unchanged");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Damage a settings file the readers fall back over (#408): the alert shows in /api/status,
    /// yields to a failing disk while it lasts, comes back once writes go through, and clears when
    /// the file reads cleanly again.
    #[tokio::test]
    async fn config_damage_shows_until_the_file_reads_cleanly_again() {
        let root = temp_root();
        let mut app = test_app(&root);
        let path = app.cfg.config_dir.join("orgs.json");
        std::fs::create_dir_all(&app.cfg.config_dir).unwrap();
        std::fs::write(&path, b"broken").unwrap();

        assert!(app.all_org_settings().is_empty(), "the reader still answers");
        let shown = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(shown["kind"], "load_damage");
        assert_eq!(shown["ok"], true, "writes still go through; the kind keeps it shown");
        assert!(
            shown["message"].as_str().unwrap().contains("orgs.json")
                && shown["message"].as_str().unwrap().contains("defaults are in effect"),
            "{shown}"
        );

        // Live damage outranks the sticky startup damage: only the live one can clear.
        Arc::get_mut(&mut app).unwrap().load_damage = Some(StorageAlert {
            kind: StorageAlertKind::LoadDamage,
            message: "sessions.json was moved aside at startup".into(),
            ts: Utc::now(),
            failures: 1,
            recovered_at: None,
        });
        assert!(
            storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default())["message"]
                .as_str()
                .unwrap()
                .contains("orgs.json"),
            "the alert that can still clear is the one shown"
        );

        // A failing disk outranks it while it lasts, then it comes back unchanged.
        app.storage_failed("save the session list", &anyhow!("disk is full")).await;
        let failing = storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default());
        assert_eq!(failing["kind"], "write");
        app.storage_succeeded().await;
        assert_eq!(
            storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default())["kind"],
            "load_damage"
        );

        std::fs::write(&path, b"{}").unwrap();
        assert!(app.all_org_settings().is_empty());
        assert!(
            app.config_damage.lock().unwrap().is_none(),
            "a clean read clears the alert that named the file"
        );
        assert!(
            storage_status(app.shown_storage_alert().await, &reclaim::FreeSpaceVerdict::default())["message"]
                .as_str()
                .unwrap()
                .contains("sessions.json"),
            "with the live alert cleared, the startup damage shows again"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A platform whose assets vendor no mesh binaries — every Mac — is a fact to explain, not a
    /// fault (#128). Before the fix the explanation sat in `error`, and everything the web UI reads
    /// from `error` turns red, so every Mac's Runtime section showed "Something is missing" for its
    /// whole life. The real handler over the stock fixture does exactly that shape: mesh enabled,
    /// no assets, nothing broken.
    #[tokio::test]
    async fn the_status_mesh_is_unavailable_with_a_null_error_where_no_binaries_are_vendored() {
        let root = temp_root();
        let app = test_app(&root);
        let Json(payload) = status(
            State(app),
            Extension(auth::Authenticated(true)),
            Query(StatusQuery { fresh: None }),
        )
        .await;
        let mesh = &payload["mesh"];
        assert_eq!(mesh["enabled"], true);
        assert_eq!(mesh["provider"], "headscale");
        assert_eq!(mesh["state"], "unavailable", "{mesh}");
        assert!(mesh["detail"].as_str().is_some_and(|d| !d.is_empty()), "{mesh}");
        assert!(
            mesh["error"].is_null(),
            "a non-null error here is the regression: the UI paints it as a fault: {mesh}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The other half of the contract: a mesh that genuinely cannot be built is a fault, and the
    /// reason lands in `error`, the field the web UI reads as "something is missing".
    #[tokio::test]
    async fn a_mesh_that_cannot_be_built_is_reported_as_an_error_carrying_the_reason() {
        let root = temp_root();
        let assets = root.join("assets");
        for rel in [
            "vendor/headscale",
            "vendor/tailscale/tailscale",
            "vendor/tailscale/tailscaled",
        ] {
            let path = assets.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
        }
        let broken: Result<Value> = Err(anyhow!("headscale refused to start"));
        let value = mesh_status(&ModulesConfig::default(), Some(&assets), async move { broken }).await;
        assert_eq!(value["state"], "error", "{value}");
        assert_eq!(value["error"], "headscale refused to start", "{value}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A mesh branch that never resolves must not take `/api/status` with it either: the bound here
    /// covers the wait on `Mesh`'s `running` lock, which a boot can hold across calls nothing else
    /// bounds. Paused time fires the 15s branch limit without waiting for it; a regression to an
    /// unbounded await would hang this test instead of passing it.
    #[tokio::test(start_paused = true)]
    async fn a_mesh_branch_that_never_resolves_is_reported_as_an_error_when_the_branch_limit_fires() {
        let root = temp_root();
        let assets = root.join("assets");
        for rel in [
            "vendor/headscale",
            "vendor/tailscale/tailscale",
            "vendor/tailscale/tailscaled",
        ] {
            let path = assets.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
        }
        let started = std::time::Instant::now();
        let value = mesh_status(&ModulesConfig::default(), Some(&assets), std::future::pending()).await;
        assert_eq!(value["enabled"], true, "{value}");
        assert_eq!(value["provider"], "headscale", "{value}");
        assert_eq!(value["state"], "error", "{value}");
        let message = value["error"].as_str().unwrap_or_default();
        assert!(message.contains("timed out"), "{value}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the branch answered in {:?}; the pending future was waited out",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A probe that never returns must not take `/api/status` with it: the handler still answers,
    /// with the probe reported as absent. Paused time fires the 5s probe limit without waiting for
    /// it; a regression to an unbounded exec would hang this test instead of passing it.
    #[tokio::test(start_paused = true)]
    async fn the_status_handler_still_answers_when_the_msb_probe_never_returns() {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_root();
        let mut app = test_app(&root);
        let wedged = root.join("wedged-msb");
        std::fs::write(&wedged, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&wedged, std::fs::Permissions::from_mode(0o755)).unwrap();
        Arc::get_mut(&mut app).unwrap().cfg.msb = wedged.display().to_string();
        let started = std::time::Instant::now();
        let Json(payload) = status(
            State(app),
            Extension(auth::Authenticated(true)),
            Query(StatusQuery { fresh: None }),
        )
        .await;
        assert!(
            payload["sandbox"]["msb_version"].is_null(),
            "the wedged probe is reported as absent, not hung: {payload}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the handler answered in {:?}; the probe was waited out",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The `host` object rides every poll: a stable id, the admission numbers, and the RFC 3339
    /// `checked_at` that says how fresh the rest of it is. On this Linux machine the probe also
    /// fills most of the measurables; the contract only promises `id` and `checked_at` wherever the
    /// mothership runs.
    #[tokio::test]
    async fn the_status_payload_carries_a_host_object_with_the_microvm_counts() {
        let root = temp_root();
        let app = test_app(&root);
        let Json(payload) = status(
            State(app),
            Extension(auth::Authenticated(true)),
            Query(StatusQuery { fresh: None }),
        )
        .await;
        let host = &payload["host"];
        assert!(host["id"].as_str().is_some_and(|s| !s.is_empty()), "{host}");
        assert!(
            host["checked_at"].is_string(),
            "checked_at is the RFC 3339 string the contract sends: {host}"
        );
        assert!(host["microvms_live"].is_u64(), "{host}");
        assert!(host["microvms_ceiling"].is_u64(), "{host}");
        assert!(
            !payload["host"].is_null(),
            "host is a top-level key, not nested under runtime: {payload}"
        );
        assert!(
            payload["runtime"]["host"].is_null(),
            "Runtime carries no host of its own (`os` aside): host lives only at the top level: {payload}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
