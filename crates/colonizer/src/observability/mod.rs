//! The `observability` module kind (#840): settings, and the Prometheus endpoint (#852).
//!
//! #840 declared the kind, its two providers (`otlp` and `file`), their settings schema and the
//! rules a save must pass. #852 adds the one surface a mothership can serve without an exporter:
//! `GET /metrics`, read at scrape time straight from in-memory state, behind the `prometheus`
//! switch and the install-wide read token.

pub mod metrics;
pub mod settings;

pub(crate) use metrics::FEATURE;

// The exporter's lowest layer (#842): a rotation- and truncation-safe jsonl tailer with a durable
// cursor. Nothing calls it yet (the multi-source tailer, #843, wires it in), so outside the tests
// it is dead code.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod cursor;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod state;

#[cfg(test)]
mod tests;
