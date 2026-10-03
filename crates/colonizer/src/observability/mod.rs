//! The `observability` module kind (#840): settings only.
//!
//! This issue declares the kind, the two providers (`otlp` and `file`), their settings schema and
//! the rules a save must pass. Nothing here sends a byte, and nothing here reads an environment
//! variable — the exporter that consumes [`settings::ExporterConfig`] lands in a later issue, and
//! the header values it will send come from the `observability-headers` secret, which this build
//! only registers.

pub mod settings;

#[cfg(test)]
mod tests;
