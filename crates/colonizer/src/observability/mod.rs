//! The `observability` module kind (#840), the mothership's half of the exporter (#841, #849,
//! #850, #854) and the Prometheus endpoint (#852).
//!
//! The exporter itself is the separate `colonizer-observability` add-on binary, never linked here
//! (docs/design/observability.md): this side resolves the effective config ([`env`]), writes the
//! contract the add-on reads, hands it the `observability-headers` secret on stdin, supervises the
//! child process and serves the status and "send a test event" routes ([`supervisor`]). Nothing
//! runs, and nothing leaves the machine, unless the module is saved and enabled or the operator set
//! `COLONIZER_OBSERVABILITY=on`.
//!
//! #840 declared the kind, its two providers (`otlp` and `file`) and the rules a save must pass.
//! #852 adds the one surface a mothership can serve without an exporter: `GET /metrics`
//! ([`metrics`]), read at scrape time straight from in-memory state, behind the `prometheus`
//! switch and the install-wide read token. It is served in-process and has nothing to do with the
//! add-on: no child process, no push, nothing leaves the machine on its account.

pub mod env;
pub mod metrics;
pub(crate) mod oplog;
pub(crate) mod reset;
pub mod settings;
pub(crate) mod supervisor;

mod access;
mod disk_guard;

pub(crate) use metrics::FEATURE;
pub(crate) use supervisor::{routes, start_tasks};

/// The one word `GET /api/status` carries about observability (#850): `off`, `ok` or `degraded`.
///
/// The mapping is deliberately flat, because the page that reads it has no room for the nuance the
/// full [`supervisor`] status route reports:
///
/// * `off` — [`env::resolve`] said the module is off, or no module exists at all. This is the
///   steady state of an install that never turned the exporter on, not a fault.
/// * `degraded` — the module is saved and meant to be running but isn't healthy: the config does
///   not resolve ([`env::Resolved::Invalid`]), the child sits in a non-running state (`refused`,
///   `restarting`, `failed`…), or `status.json` says the add-on is retrying, auth-failed, or
///   backing a signal off. The full route says which; this says only that it is worth opening.
/// * `ok` — configured, resolved, child running and `status.json` reporting no complaint.
///
/// It must never slow the status poll down: it resolves the config with a default identity (the
/// Off/Invalid/On verdict never reads it, so the poll never runs the hostname probe), and its only
/// awaits are the modules read lock — never anything shared with the event or session write paths.
pub(crate) async fn compact_status(app: &crate::App) -> &'static str {
    use env::{Identity, Resolved};

    // The same read `supervisor::resolve` makes — module choice and secret — but with a default
    // identity: the Off/Invalid/On verdict never looks at it (only resource attributes do), so the
    // status poll never runs the hostname probe or the fleet read on observability's account.
    let module = app.modules.read().await.observability.clone();
    let secret = crate::util::read_secret(&app.cfg.config_dir.join("observability-headers"));
    match env::resolve(
        module.as_ref(),
        &|name| std::env::var(name).ok(),
        secret,
        &Identity::default(),
    ) {
        Resolved::Off(_) => "off",
        Resolved::Invalid(_) => "degraded",
        Resolved::On(_) => {
            let data_dir = app.cfg.data_dir.clone();
            if !matches!(supervisor::runtime_state(&data_dir), "running" | "starting") {
                return "degraded";
            }
            // The add-on's own word: retrying or auth-failed overall, or any signal backing off
            // or auth-failed. Anything else — including a status.json it has not written yet —
            // leaves the install ok.
            let status: Option<serde_json::Value> = std::fs::read(supervisor::status_path(&data_dir))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let complaining = status.is_some_and(|s| {
                matches!(s["state"].as_str(), Some("retrying" | "auth_failed"))
                    || s["signals"].as_object().is_some_and(|signals| {
                        signals
                            .values()
                            .any(|v| matches!(v["state"].as_str(), Some("backing_off" | "auth_failed")))
                    })
            });
            if complaining { "degraded" } else { "ok" }
        }
    }
}

#[cfg(test)]
mod tests;
