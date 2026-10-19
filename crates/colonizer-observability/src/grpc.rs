//! OTLP/gRPC (#851): the transport behind `protocol = "grpc"` and this crate's `otlp-grpc` cargo
//! feature. Each signal gets its own lazy tonic channel to the contract's endpoint for it — its
//! per-signal endpoint when one is set, else the base, trailing `/` trimmed and no OTLP path
//! appended (the RPC method is the path). `https://` connects with TLS (webpki roots over ring,
//! the stack reqwest already brings); `http://` is plaintext h2c; anything else is refused at
//! startup. The exporter loop never sees any of this: answers sort into the HTTP transport's
//! [`Outcome`], by the OTLP/gRPC spec's status table (which `tests.rs` pins):
//!
//! | gRPC status | Outcome |
//! | --- | --- |
//! | `Cancelled`, `DeadlineExceeded`, `Aborted`, `OutOfRange`, `DataLoss`, `Unavailable` | `Retry` (backoff) |
//! | `ResourceExhausted` with a `google.rpc.RetryInfo` detail | `Retry` after the detail's delay, capped at [`MAX_RETRY_AFTER`] |
//! | `ResourceExhausted` without one | `Refused` |
//! | `InvalidArgument` | `Refused`: the exporter bisects the batch to the record at fault |
//! | `Unauthenticated`, `PermissionDenied` | `Unauthorized`: the batch waits for a working key |
//! | `Unknown`, `NotFound`, `Unimplemented`, `AlreadyExists`, `FailedPrecondition`, `Internal` | `Refused`: the spec says not retryable, and `Refused` drains instead of stalling |
//! | `Ok` | `Retry`, defensively: an `Ok` code never travels as an error |
//!
//! Transport failures arrive as `Unavailable` statuses (tonic reports its connect and I/O errors
//! that way), so the network errors retry exactly like the HTTP transport's. Error strings go
//! through the same [`scrub_error`], so neither transport can quote a header value the other
//! could not.

use crate::contract::Settings;
use crate::encode::{Encoding, Request};
use crate::proto::collector::logs::v1::logs_service_client::LogsServiceClient;
use crate::proto::collector::metrics::v1::metrics_service_client::MetricsServiceClient;
use crate::proto::collector::trace::v1::trace_service_client::TraceServiceClient;
use crate::transport::{MAX_RETRY_AFTER, Outcome, cap, partial_success, scrub_error};
use prost::Message as _;
use std::time::Duration;
use tonic::codec::CompressionEncoding;
use tonic::metadata::{AsciiMetadataKey, AsciiMetadataValue, MetadataMap};
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Status};

/// Headers the channel owns: forwarding one would corrupt the call (tonic sets `content-type` and
/// `te` itself, and the message framing owns the rest).
const OWNED_HEADERS: &[&str] = &["content-type", "content-length", "content-encoding", "host", "te"];

/// One OTLP/gRPC destination: a client per signal, all carrying the operator's metadata.
pub(crate) struct GrpcTransport {
    logs: LogsServiceClient<Channel>,
    traces: TraceServiceClient<Channel>,
    metrics: MetricsServiceClient<Channel>,
    urls: Urls,
    metadata: MetadataMap,
    /// The raw pairs, for [`scrub_error`]; only scrubbed text ever leaves the transport.
    headers: Vec<(String, String)>,
    timeout: Duration,
}

/// The endpoint each signal's channel connects to, for error messages that name it like the HTTP
/// transport's do.
struct Urls {
    logs: String,
    traces: String,
    metrics: String,
}

impl std::fmt::Debug for GrpcTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<_> = self.headers.iter().map(|(n, _)| format!("{n}=<redacted>")).collect();
        f.debug_struct("GrpcTransport")
            .field("logs", &self.urls.logs)
            .field("traces", &self.urls.traces)
            .field("metrics", &self.urls.metrics)
            .field("headers", &names)
            .finish()
    }
}

impl GrpcTransport {
    /// Builds the three clients. Nothing connects yet: the channels are lazy, like the HTTP
    /// transport's reqwest pool, so a dead endpoint surfaces as a retryable `Unavailable`, not a
    /// startup failure. Like every tonic channel, the lazy ones spawn their connection tasks at
    /// construction, so this runs inside the exporter's tokio runtime, where the HTTP transport
    /// builds its client too.
    pub(crate) fn new(settings: &Settings, headers: Vec<(String, String)>) -> Result<GrpcTransport, String> {
        let metadata = metadata_map(&headers)?;
        let urls = Urls {
            logs: base_url(settings, settings.logs_endpoint.as_deref())?,
            traces: base_url(settings, settings.traces_endpoint.as_deref())?,
            metrics: base_url(settings, settings.metrics_endpoint.as_deref())?,
        };
        let timeout = Duration::from_secs(settings.timeout_secs.clamp(1, 120));
        // gzip by the HTTP transport's rule (`compression != "none"`); gzip answers accepted too,
        // so a backend that compresses is readable.
        let gzip = settings.compression != "none";
        let (mut logs, mut traces, mut metrics) = (
            LogsServiceClient::new(channel(&urls.logs)?),
            TraceServiceClient::new(channel(&urls.traces)?),
            MetricsServiceClient::new(channel(&urls.metrics)?),
        );
        if gzip {
            logs = logs.send_compressed(CompressionEncoding::Gzip);
            traces = traces.send_compressed(CompressionEncoding::Gzip);
            metrics = metrics.send_compressed(CompressionEncoding::Gzip);
        }
        let (logs, traces, metrics) = (
            logs.accept_compressed(CompressionEncoding::Gzip),
            traces.accept_compressed(CompressionEncoding::Gzip),
            metrics.accept_compressed(CompressionEncoding::Gzip),
        );
        Ok(GrpcTransport {
            logs,
            traces,
            metrics,
            urls,
            metadata,
            headers,
            timeout,
        })
    }

    /// Sends one request and classifies the answer, exactly as the HTTP transport does.
    pub(crate) async fn send(&self, request: &Request) -> Outcome {
        match request {
            Request::Logs(r) => {
                let mut call = tonic::Request::new(r.clone());
                self.decorate(&mut call);
                match self.logs.clone().export(call).await {
                    Ok(response) => self.sent(request, &response.into_inner().encode_to_vec()),
                    Err(status) => self.failed(&self.urls.logs, &status),
                }
            }
            Request::Traces(r) => {
                let mut call = tonic::Request::new(r.clone());
                self.decorate(&mut call);
                match self.traces.clone().export(call).await {
                    Ok(response) => self.sent(request, &response.into_inner().encode_to_vec()),
                    Err(status) => self.failed(&self.urls.traces, &status),
                }
            }
            Request::Metrics(r) => {
                let mut call = tonic::Request::new(r.clone());
                self.decorate(&mut call);
                match self.metrics.clone().export(call).await {
                    Ok(response) => self.sent(request, &response.into_inner().encode_to_vec()),
                    Err(status) => self.failed(&self.urls.metrics, &status),
                }
            }
        }
    }

    /// Every call carries the operator's metadata and the timeout the HTTP transport's client
    /// would enforce.
    fn decorate(&self, call: &mut tonic::Request<impl Sized>) {
        *call.metadata_mut() = self.metadata.clone();
        call.set_timeout(self.timeout);
    }

    /// A `Ok` answer's partial success, decoded by the one decoder the HTTP transport uses (the
    /// response message re-encoded, so both paths read the same bytes). `bytes` is the protobuf
    /// message as sent; tonic's framing and compression are the channel's.
    fn sent(&self, request: &Request, body: &[u8]) -> Outcome {
        let (rejected, message) = partial_success(request, Encoding::Protobuf, body);
        Outcome::Sent {
            rejected,
            message: message.map(|m| scrub_error(&self.headers, &cap(&m))),
            bytes: request.encoded_len(Encoding::Protobuf) as u64,
        }
    }

    /// A status answer, sorted by [`classify_status`] and scrubbed by the shared scrubber.
    fn failed(&self, url: &str, status: &Status) -> Outcome {
        let message = scrub_error(
            &self.headers,
            &format!("{url} answered {}: {}", code_name(status.code()), status.message()),
        );
        match classify_status(status) {
            GrpcClass::Retry(after) => Outcome::Retry {
                message,
                after,
                reason: None,
            },
            GrpcClass::Unauthorized => Outcome::Unauthorized {
                // The gRPC code's number (0-16), where the HTTP transport carries a status code.
                status: status.code() as u16,
                message,
            },
            GrpcClass::Refused => Outcome::Refused {
                status: status.code() as u16,
                message,
            },
        }
    }
}

/// How a gRPC error status is handled: the module table's three columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrpcClass {
    /// Worth retrying: after a backoff, or the `RetryInfo` delay when one rode along.
    Retry(Option<Duration>),
    /// The credential was refused: the records are fine, the key is not.
    Unauthorized,
    /// About this request's records: the exporter bisects it.
    Refused,
}

/// Sorts a gRPC status by the module table. `ResourceExhausted` only earns a retry when the
/// backend said how long to wait (a rich-error `RetryInfo`); everything else follows the OTLP
/// spec's retryable-or-drop split.
pub(crate) fn classify_status(status: &Status) -> GrpcClass {
    match status.code() {
        Code::Cancelled | Code::DeadlineExceeded | Code::Aborted | Code::OutOfRange | Code::DataLoss | Code::Unavailable => {
            GrpcClass::Retry(None)
        }
        Code::ResourceExhausted => match retry_info_delay(status.details()) {
            Some(delay) => GrpcClass::Retry(Some(delay)),
            None => GrpcClass::Refused,
        },
        Code::Unauthenticated | Code::PermissionDenied => GrpcClass::Unauthorized,
        // Never an error; kept retryable so a misbehaving backend cannot stall the pipeline.
        Code::Ok => GrpcClass::Retry(None),
        Code::InvalidArgument
        | Code::Unknown
        | Code::NotFound
        | Code::Unimplemented
        | Code::AlreadyExists
        | Code::FailedPrecondition
        | Code::Internal => GrpcClass::Refused,
    }
}

/// A `ResourceExhausted` status's `google.rpc.RetryInfo` delay, when the rich-error detail rode
/// along: [`Status::details`] is a serialized `google.rpc.Status` whose `details` hold the `Any`s,
/// decoded by hand in [`rich_error`] so no extra crate is pulled in. A negative delay is no delay;
/// the delay is capped like the HTTP transport caps `Retry-After`.
pub(crate) fn retry_info_delay(details: &[u8]) -> Option<Duration> {
    if details.is_empty() {
        return None;
    }
    let status = rich_error::Status::decode(details).ok()?;
    let any = status
        .details
        .iter()
        .find(|any| any.type_url.ends_with("google.rpc.RetryInfo"))?;
    let delay = rich_error::RetryInfo::decode(any.value.as_slice()).ok()?.retry_delay?;
    if delay.seconds < 0 || delay.nanos < 0 {
        return None;
    }
    Some(Duration::new(delay.seconds as u64, delay.nanos as u32).min(MAX_RETRY_AFTER))
}

/// The `google.rpc` rich-error model's messages, as prost structs: hand-decoded so reading a
/// `RetryInfo` costs no `tonic-types` dependency.
mod rich_error {
    use prost::Message;

    #[derive(Clone, PartialEq, Message)]
    pub struct Status {
        #[prost(int32, tag = "1")]
        pub code: i32,
        #[prost(string, tag = "2")]
        pub message: String,
        #[prost(message, repeated, tag = "3")]
        pub details: Vec<Any>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct Any {
        #[prost(string, tag = "1")]
        pub type_url: String,
        #[prost(bytes = "vec", tag = "2")]
        pub value: Vec<u8>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct RetryInfo {
        #[prost(message, optional, tag = "1")]
        pub retry_delay: Option<Duration>,
    }

    /// `google.protobuf.Duration`, not `std`'s: protobuf allows it to be negative, which
    /// [`super::retry_info_delay`] treats as no delay.
    #[derive(Clone, Copy, PartialEq, Message)]
    pub struct Duration {
        #[prost(int64, tag = "1")]
        pub seconds: i64,
        #[prost(int32, tag = "2")]
        pub nanos: i32,
    }
}

/// The endpoint one signal's channel connects to: its own endpoint when the contract set one, else
/// the base; trailing `/` trimmed. Unlike [`Settings::url`] no OTLP path is appended: the RPC
/// method is the path.
fn base_url(settings: &Settings, own: Option<&str>) -> Result<String, String> {
    let url = own.filter(|u| !u.is_empty()).unwrap_or(&settings.endpoint);
    let url = url.trim_end_matches('/');
    if url.is_empty() {
        return Err("no endpoint: the contract sets no base endpoint for gRPC".into());
    }
    Ok(url.to_string())
}

/// One lazy channel, TLS for `https://` and plaintext h2c for `http://`; like the HTTP transport,
/// the endpoint carries no credential (the mothership refuses userinfo and queries), so error
/// messages may name it. The scheme is checked before tonic sees the URL: `Endpoint::from_shared`
/// would happily dial a `unix:` endpoint instead, and the contract's endpoints are network ones.
fn channel(url: &str) -> Result<Channel, String> {
    let make = || Endpoint::from_shared(url.to_string()).map_err(|e| format!("cannot use {url} as a gRPC endpoint: {e}"));
    let endpoint = match url.split_once("://").map(|(scheme, _)| scheme) {
        Some("https") => make()?
            .tls_config(ClientTlsConfig::new().with_webpki_roots())
            .map_err(|e| format!("cannot ready {url} for TLS: {e}"))?,
        Some("http") => make()?,
        _ => return Err(format!("gRPC endpoint {url} must start with http:// or https://")),
    };
    Ok(endpoint.connect_lazy())
}

/// The operator's headers as gRPC metadata. Headers the channel owns are dropped (names compare
/// case-insensitively, like HTTP's); a name or value metadata cannot carry is refused, naming the
/// header and never its value.
fn metadata_map(headers: &[(String, String)]) -> Result<MetadataMap, String> {
    let mut map = MetadataMap::new();
    for (name, value) in headers {
        if OWNED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        let key =
            AsciiMetadataKey::from_bytes(name.as_bytes()).map_err(|_| format!("header {name} is not valid gRPC metadata"))?;
        // tonic's own check lets opaque octets through (http's `HeaderValue` is that lenient);
        // gRPC metadata values are visible ASCII, so the check is made here.
        if !value.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
            return Err(format!("header {name} has a value gRPC metadata cannot carry"));
        }
        let value = AsciiMetadataValue::try_from(value.as_str())
            .map_err(|_| format!("header {name} has a value gRPC metadata cannot carry"))?;
        map.insert(key, value);
    }
    Ok(map)
}

/// The gRPC code's wire name, lowercase, like the code reads in an OTLP/HTTP error's status line.
fn code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "ok",
        Code::Cancelled => "cancelled",
        Code::Unknown => "unknown",
        Code::InvalidArgument => "invalid-argument",
        Code::DeadlineExceeded => "deadline-exceeded",
        Code::NotFound => "not-found",
        Code::AlreadyExists => "already-exists",
        Code::PermissionDenied => "permission-denied",
        Code::ResourceExhausted => "resource-exhausted",
        Code::FailedPrecondition => "failed-precondition",
        Code::Aborted => "aborted",
        Code::OutOfRange => "out-of-range",
        Code::Unimplemented => "unimplemented",
        Code::Internal => "internal",
        Code::Unavailable => "unavailable",
        Code::DataLoss => "data-loss",
        Code::Unauthenticated => "unauthenticated",
    }
}

#[cfg(test)]
mod tests;
