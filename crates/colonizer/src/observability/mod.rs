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
pub mod settings;
pub(crate) mod supervisor;

pub(crate) use metrics::FEATURE;
pub(crate) use supervisor::{routes, start_tasks};

#[cfg(test)]
mod tests;
