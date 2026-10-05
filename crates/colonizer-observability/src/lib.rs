//! Colonizer's observability add-on (docs/design/observability.md): exports a mothership's logs,
//! traces and metrics to the operator's own OTLP backend. It ships as its own binary and is never
//! linked into the mothership, so none of OpenTelemetry, protobuf or gzip lands in a default install.
//!
//! This is the foundation (#844) the readers and mappers build on:
//!
//! - [`policy`] is the only way a string reaches an export: allowlisted attribute keys per record
//!   kind, the content tier behind a gate, redaction again at export time, byte caps with a
//!   truncation marker, hashed repository names and image placeholders.
//! - [`batch`] packs the policy's records into OTLP requests that never exceed a byte budget.
//! - [`encode`] turns a request into OTLP protobuf or OTLP/JSON bytes, and gzips them.
//! - [`hashing`] keeps the per-install key `repo_names = hashed` hashes with.
//!
//! No OpenTelemetry SDK runs here: the OTLP messages are built and encoded directly.

pub mod batch;
pub mod encode;
pub mod hashing;
pub mod policy;
#[cfg(test)]
pub(crate) mod testkit;

/// The generated OTLP message types, for the readers and mappers that fill them.
pub use opentelemetry_proto::tonic as proto;
