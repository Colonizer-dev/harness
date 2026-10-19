//! Colonizer's observability add-on (docs/design/observability.md): exports a mothership's logs,
//! traces and metrics to the operator's own OTLP backend. It ships as its own binary and is never
//! linked into the mothership, so none of OpenTelemetry, protobuf or gzip lands in a default install.
//!
//! The foundation (#844):
//!
//! - [`policy`] is the only way a string reaches an export: allowlisted attribute keys per record
//!   kind, the content tier behind a gate, redaction again at export time, byte caps with a
//!   truncation marker, hashed repository names and image placeholders.
//! - [`batch`] packs the policy's records into OTLP requests that never exceed a byte budget.
//! - [`encode`] turns a request into OTLP protobuf or OTLP/JSON bytes, and gzips them.
//! - [`hashing`] keeps the per-install key `repo_names = hashed` hashes with.
//!
//! The first exporter on top of it:
//!
//! - [`contract`] is what the mothership hands over: `exporter.json` and the headers on stdin.
//! - `cursor` and `state` tail a jsonl file safely across rotation and truncation (#842), and
//!   [`sources`] lists which files are tailed for which signal; [`tailer`] reads them all fairly,
//!   within a budget, with gap records for every hole (#843).
//! - [`map`] turns ledger lines into log records and [`metrics`] folds them into metric series;
//!   `traces` rebuilds each colony's trace from its events (#846).
//! - [`transport`] is OTLP/HTTP, and [`exporter`] is the loop: read, map, send, commit on ack.
//! - [`backfill`] replays the ledgers' on-disk history to a destination in one shot (#858).
//! - `archive_source` reads the tar+zstd bundles of colonies the disk no longer holds (#859), and
//!   [`export`] writes an offline OTLP/JSON file instead of sending anything.
//!
//! No OpenTelemetry SDK runs here: the OTLP messages are built and encoded directly.

pub(crate) mod archive_source;
pub mod backfill;
pub mod batch;
pub mod contract;
pub(crate) mod cursor;
/// The send-bisect delivery loop backfill and the fleet relay share.
pub(crate) mod delivery;
pub mod encode;
pub mod export;
pub mod exporter;
/// The OTLP/gRPC transport (#851), behind the `otlp-grpc` cargo feature.
#[cfg(feature = "otlp-grpc")]
pub mod grpc;
pub mod hashing;
pub mod health;
pub mod loki;
pub mod map;
pub mod metrics;
#[cfg(test)]
mod observability_conformance;
pub mod policy;
/// The fleet relay (#861): members' pushed history to the owner's own backend.
pub mod relay;
pub(crate) mod sources;
pub(crate) mod state;
pub(crate) mod tailer;
#[cfg(test)]
pub(crate) mod testkit;
pub(crate) mod traces;
pub mod transport;

/// The generated OTLP message types, for the readers and mappers that fill them.
pub use opentelemetry_proto::tonic as proto;

/// Writes `bytes` to `path` atomically: a private temp file beside it, synced, then renamed over
/// it, so a reader sees the old file or the new one and never half of either.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
