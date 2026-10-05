//! OTLP request encoding: protobuf (the OTLP/HTTP default), OTLP/JSON, and gzip for
//! `Content-Encoding: gzip`.
//!
//! OTLP/JSON follows the spec's JSON mapping (lowerCamelCase fields, trace and span ids as lowercase
//! hex, 64-bit integers as strings, enums as their integer values). `opentelemetry-proto`'s
//! `with-serde` gets most of that right; [`to_json`] fixes the two places it does not — a metric
//! point's or exemplar's `asInt` comes out as a JSON number, and an absent optional message as
//! `null` — and the golden in `tests/fixtures/otlp/` pins the result.

use crate::proto::collector::logs::v1::ExportLogsServiceRequest;
use crate::proto::collector::metrics::v1::ExportMetricsServiceRequest;
use crate::proto::collector::trace::v1::ExportTraceServiceRequest;
use flate2::Compression;
use flate2::write::GzEncoder;
use prost::Message;
use serde_json::Value;
use std::io::Write;

/// The wire format of an export request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoding {
    #[default]
    Protobuf,
    Json,
}

/// One OTLP export request, of one signal.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Logs(ExportLogsServiceRequest),
    Traces(ExportTraceServiceRequest),
    Metrics(ExportMetricsServiceRequest),
}

impl Request {
    /// The request's bytes in `encoding`.
    pub fn encode(&self, encoding: Encoding) -> Vec<u8> {
        match encoding {
            Encoding::Protobuf => to_protobuf(self),
            Encoding::Json => to_json(self),
        }
    }

    /// The length of [`Request::encode`]'s output. Exact; protobuf is sized without encoding.
    pub fn encoded_len(&self, encoding: Encoding) -> usize {
        match (encoding, self) {
            (Encoding::Protobuf, Request::Logs(r)) => r.encoded_len(),
            (Encoding::Protobuf, Request::Traces(r)) => r.encoded_len(),
            (Encoding::Protobuf, Request::Metrics(r)) => r.encoded_len(),
            (Encoding::Json, _) => to_json(self).len(),
        }
    }

    /// The number of log records, spans or metrics the request carries.
    pub fn len(&self) -> usize {
        match self {
            Request::Logs(r) => r
                .resource_logs
                .iter()
                .flat_map(|r| &r.scope_logs)
                .map(|s| s.log_records.len())
                .sum(),
            Request::Traces(r) => r
                .resource_spans
                .iter()
                .flat_map(|r| &r.scope_spans)
                .map(|s| s.spans.len())
                .sum(),
            Request::Metrics(r) => r
                .resource_metrics
                .iter()
                .flat_map(|r| &r.scope_metrics)
                .map(|s| s.metrics.len())
                .sum(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The OTLP/HTTP path the request is POSTed to.
    pub fn path(&self) -> &'static str {
        match self {
            Request::Logs(_) => "/v1/logs",
            Request::Traces(_) => "/v1/traces",
            Request::Metrics(_) => "/v1/metrics",
        }
    }
}

/// The request as OTLP protobuf (`application/x-protobuf`).
pub fn to_protobuf(request: &Request) -> Vec<u8> {
    match request {
        Request::Logs(r) => r.encode_to_vec(),
        Request::Traces(r) => r.encode_to_vec(),
        Request::Metrics(r) => r.encode_to_vec(),
    }
}

/// The request as OTLP/JSON (`application/json`), compact.
pub fn to_json(request: &Request) -> Vec<u8> {
    json_bytes(to_json_value(request))
}

/// The request as an OTLP/JSON value.
pub fn to_json_value(request: &Request) -> Value {
    let value = match request {
        Request::Logs(r) => serde_json::to_value(r),
        Request::Traces(r) => serde_json::to_value(r),
        Request::Metrics(r) => serde_json::to_value(r),
    };
    // The generated types serialize infallibly: string keys, no maps keyed by anything else.
    fix_json(value.expect("OTLP messages serialize to JSON"))
}

/// A serialized OTLP message (or part of one) with [`to_json`]'s fixes applied, as bytes.
pub(crate) fn json_bytes(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).expect("a JSON value serializes")
}

/// `with-serde`'s output brought in line with the OTLP/JSON mapping: `null`s dropped (an absent
/// message is omitted, not null) and `asInt` as a string, like every other 64-bit integer.
pub(crate) fn fix_json(mut value: Value) -> Value {
    fix(&mut value);
    value
}

fn fix(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            for (key, v) in map.iter_mut() {
                if key == "asInt"
                    && let Value::Number(n) = v
                {
                    *v = Value::String(n.to_string());
                } else {
                    fix(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(fix),
        _ => {}
    }
}

/// `bytes` gzipped, for an OTLP/HTTP request with `Content-Encoding: gzip`.
pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::with_capacity(bytes.len() / 4 + 64), Compression::default());
    // Writing into a Vec cannot fail.
    encoder.write_all(bytes).expect("gzip into memory");
    encoder.finish().expect("gzip into memory")
}

#[cfg(test)]
mod tests;
