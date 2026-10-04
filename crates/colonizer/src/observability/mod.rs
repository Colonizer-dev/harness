//! The `observability` module kind (#840): settings only.
//!
//! This issue declares the kind, the two providers (`otlp` and `file`), their settings schema and
//! the rules a save must pass. Nothing here sends a byte, and nothing here reads an environment
//! variable — the exporter that consumes [`settings::ExporterConfig`] lands in a later issue, and
//! the header values it will send come from the `observability-headers` secret, which this build
//! only registers.

pub mod settings;

// The exporter's lowest layer (#842): a rotation- and truncation-safe jsonl tailer with a durable
// cursor. Nothing calls it yet (the multi-source tailer, #843, wires it in), so outside the tests
// it is dead code.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod cursor;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod state;

#[cfg(test)]
mod tests;
