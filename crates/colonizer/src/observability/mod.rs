//! The `observability` module kind (#840) and the mothership's half of the exporter (#841, #849,
//! #850, #854).
//!
//! The exporter itself is the separate `colonizer-observability` add-on binary, never linked here
//! (docs/design/observability.md): this side resolves the effective config ([`env`]), writes the
//! contract the add-on reads, hands it the `observability-headers` secret on stdin, supervises the
//! child process and serves the status and "send a test event" routes ([`supervisor`]). Nothing
//! runs, and nothing leaves the machine, unless the module is saved and enabled or the operator set
//! `COLONIZER_OBSERVABILITY=on`.

pub mod env;
pub mod settings;
pub(crate) mod supervisor;

pub(crate) use supervisor::{routes, start_tasks};

#[cfg(test)]
mod tests;
