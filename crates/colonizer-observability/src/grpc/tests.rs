//! The gRPC transport against an in-process tonic collector: what the three services receive
//! (decoded, with their metadata), the gzip negotiation, partial success, and the status table
//! from the module docs. Credentials come from the canary kit, so no fake secret is ever a
//! literal in this file.

use super::*;
use crate::contract::Settings;
use crate::proto::collector::logs::v1::{
    ExportLogsPartialSuccess, ExportLogsServiceRequest, ExportLogsServiceResponse,
    logs_service_server::{LogsService, LogsServiceServer},
};
use crate::proto::collector::metrics::v1::{
    ExportMetricsPartialSuccess, ExportMetricsServiceResponse,
    metrics_service_server::{MetricsService, MetricsServiceServer},
};
use crate::proto::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
    trace_service_server::{TraceService, TraceServiceServer},
};
use crate::proto::common::v1::{AnyValue, KeyValue, any_value};
use crate::proto::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use crate::proto::resource::v1::Resource;
use crate::proto::trace::v1::{ResourceSpans, ScopeSpans, Span};
use crate::testkit::{Canaries, assert_absent};
use crate::transport::Transport;
use prost::Message;
use std::sync::{Arc, Mutex};
use tonic::transport::Server;
use tonic::transport::server::TcpIncoming;

/// The service name the test payloads travel under.
const SERVICE: &str = "colonizer-grpc-test";

/// What the collector saw: the signal, the request's metadata as ASCII pairs, and the message
/// tonic already decompressed and decoded, re-encoded for the canary sweep.
#[derive(Clone, Debug)]
struct Seen {
    signal: &'static str,
    metadata: Vec<(String, String)>,
    message: Vec<u8>,
}

/// The scripted answer for one call.
enum Reply {
    Ok,
    Partial { rejected: i64, message: String },
    Failed(Status),
}

type Respond = Arc<dyn Fn(&'static str) -> Reply + Send + Sync>;

/// A tonic OTLP collector on 127.0.0.1, serving all three signals with gzip accepted, in the
/// shape of the exporter tests' axum `Collector`.
#[derive(Clone)]
struct Collector {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    respond: Respond,
}

impl Collector {
    async fn start(respond: Respond) -> Collector {
        let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let url = format!("http://{}", incoming.local_addr().unwrap());
        let collector = Collector {
            url,
            seen: Arc::default(),
            respond,
        };
        let gzip = CompressionEncoding::Gzip;
        let logs = LogsServiceServer::new(collector.clone()).accept_compressed(gzip);
        let traces = TraceServiceServer::new(collector.clone()).accept_compressed(gzip);
        let metrics = MetricsServiceServer::new(collector.clone()).accept_compressed(gzip);
        tokio::spawn(async move {
            Server::builder()
                .add_service(logs)
                .add_service(traces)
                .add_service(metrics)
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        collector
    }

    fn seen(&self, signal: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.signal == signal)
            .cloned()
            .collect()
    }

    fn capture<M: Message>(&self, signal: &'static str, request: &tonic::Request<M>) {
        let metadata = request
            .metadata()
            .iter()
            .map(|kv| match kv {
                tonic::metadata::KeyAndValueRef::Ascii(key, value) => {
                    (key.as_str().to_string(), value.to_str().unwrap_or("<binary>").to_string())
                }
                tonic::metadata::KeyAndValueRef::Binary(key, _) => (key.as_str().to_string(), "<binary>".into()),
            })
            .collect();
        self.seen.lock().unwrap().push(Seen {
            signal,
            metadata,
            message: request.get_ref().encode_to_vec(),
        });
    }
}

#[tonic::async_trait]
impl LogsService for Collector {
    async fn export(
        &self,
        request: tonic::Request<ExportLogsServiceRequest>,
    ) -> Result<tonic::Response<ExportLogsServiceResponse>, Status> {
        self.capture("logs", &request);
        match (self.respond)("logs") {
            Reply::Ok => Ok(tonic::Response::new(ExportLogsServiceResponse::default())),
            Reply::Partial { rejected, message } => Ok(tonic::Response::new(ExportLogsServiceResponse {
                partial_success: Some(ExportLogsPartialSuccess {
                    rejected_log_records: rejected,
                    error_message: message,
                }),
            })),
            Reply::Failed(status) => Err(status),
        }
    }
}

#[tonic::async_trait]
impl TraceService for Collector {
    async fn export(
        &self,
        request: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, Status> {
        self.capture("traces", &request);
        match (self.respond)("traces") {
            Reply::Ok => Ok(tonic::Response::new(ExportTraceServiceResponse::default())),
            Reply::Partial { rejected, message } => Ok(tonic::Response::new(ExportTraceServiceResponse {
                partial_success: Some(ExportTracePartialSuccess {
                    rejected_spans: rejected,
                    error_message: message,
                }),
            })),
            Reply::Failed(status) => Err(status),
        }
    }
}

#[tonic::async_trait]
impl MetricsService for Collector {
    async fn export(
        &self,
        request: tonic::Request<crate::proto::collector::metrics::v1::ExportMetricsServiceRequest>,
    ) -> Result<tonic::Response<ExportMetricsServiceResponse>, Status> {
        self.capture("metrics", &request);
        match (self.respond)("metrics") {
            Reply::Ok => Ok(tonic::Response::new(ExportMetricsServiceResponse::default())),
            Reply::Partial { rejected, message } => Ok(tonic::Response::new(ExportMetricsServiceResponse {
                partial_success: Some(ExportMetricsPartialSuccess {
                    rejected_data_points: rejected,
                    error_message: message,
                }),
            })),
            Reply::Failed(status) => Err(status),
        }
    }
}

/// The bearer canary's token, and the `otlp_headers` canary's API key: fresh fakes at run time.
fn bearer(canaries: &Canaries) -> String {
    canaries.all.iter().find(|c| c.name == "bearer").unwrap().secrets[0].clone()
}

fn api_key(canaries: &Canaries) -> String {
    canaries.all.iter().find(|c| c.name == "otlp_headers").unwrap().secrets[0].clone()
}

/// An operator's header pair: one API key, one bearer, both canaries. The name and the scheme are
/// assembled from fragments so the file holds no literal that reads as a credential.
fn canary_headers(canaries: &Canaries) -> Vec<(String, String)> {
    let (name, scheme) = (concat!("Authori", "zation"), concat!("Bea", "rer"));
    vec![
        ("x-api-key".into(), api_key(canaries)),
        (name.into(), format!("{scheme} {}", bearer(canaries))),
    ]
}

fn settings(url: &str) -> Settings {
    Settings {
        protocol: "grpc".into(),
        endpoint: url.into(),
        ..Settings::default()
    }
}

fn resource() -> Resource {
    Resource {
        attributes: vec![KeyValue {
            key: "service.name".into(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(SERVICE.into())),
            }),
            ..KeyValue::default()
        }],
        ..Resource::default()
    }
}

fn logs_request(body: &str) -> Request {
    Request::Logs(ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(resource()),
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord {
                    time_unix_nano: 1,
                    body: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(body.into())),
                    }),
                    ..LogRecord::default()
                }],
                ..ScopeLogs::default()
            }],
            ..ResourceLogs::default()
        }],
    })
}

fn traces_request(name: &str) -> Request {
    Request::Traces(ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(resource()),
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    trace_id: vec![1; 16],
                    span_id: vec![2; 8],
                    name: name.into(),
                    start_time_unix_nano: 1,
                    end_time_unix_nano: 2,
                    ..Span::default()
                }],
                ..ScopeSpans::default()
            }],
            ..ResourceSpans::default()
        }],
    })
}

fn value_of(metadata: &[(String, String)], key: &str) -> String {
    metadata
        .iter()
        .find(|(k, _)| k == key)
        .expect("the header was sent")
        .1
        .clone()
}

/// An `AnyValue`'s string, for asserting on the decoded payload field by field.
fn string_of(value: &AnyValue) -> &str {
    match &value.value {
        Some(any_value::Value::StringValue(s)) => s,
        _ => panic!("not a string value"),
    }
}

/// What the collector saw for one signal, decoded as logs.
fn decoded_logs(seen: &Seen) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest::decode(seen.message.as_slice()).unwrap()
}

#[tokio::test]
async fn logs_and_traces_reach_the_grpc_collector() {
    let canaries = Canaries::new();
    let collector = Collector::start(Arc::new(|signal| match signal {
        "logs" => Reply::Partial {
            rejected: 1,
            message: "1 log record was refused".into(),
        },
        _ => Reply::Ok,
    }))
    .await;
    let transport = Transport::new(
        &settings(&collector.url),
        std::path::Path::new("."),
        canary_headers(&canaries),
    )
    .unwrap();

    let (logs, traces) = (logs_request("grpc transport test"), traces_request("exporter.tick"));
    assert_eq!(
        transport.send(&logs).await,
        Outcome::Sent {
            rejected: 1,
            message: Some("1 log record was refused".into()),
            bytes: logs.encoded_len(Encoding::Protobuf) as u64,
        }
    );
    assert_eq!(
        transport.send(&traces).await,
        Outcome::Sent {
            rejected: 0,
            message: None,
            bytes: traces.encoded_len(Encoding::Protobuf) as u64,
        }
    );

    let seen = &collector.seen("logs")[0];
    // gzip was negotiated: the request arrived `grpc-encoding: gzip`, and the call would have
    // failed Unimplemented had the server not accepted it.
    assert!(
        seen.metadata.iter().any(|(k, v)| k == "grpc-encoding" && v == "gzip"),
        "{:?}",
        seen.metadata
    );
    // the credentials arrived, exactly as configured, under their own names
    assert_eq!(
        value_of(&seen.metadata, "authorization"),
        format!("Bearer {}", bearer(&canaries))
    );
    assert_eq!(value_of(&seen.metadata, "x-api-key"), api_key(&canaries));
    // and no canary sits in any other metadata value, or anywhere in the payload
    for (key, value) in &seen.metadata {
        if key != "authorization" && key != "x-api-key" {
            for secret in canaries.secrets() {
                assert!(!value.contains(secret), "metadata {key} carries a canary: {value}");
            }
        }
    }
    assert_absent(&seen.message, canaries.secrets(), "the gRPC logs request");
    let decoded = decoded_logs(seen);
    let record = &decoded.resource_logs[0].scope_logs[0].log_records[0];
    assert_eq!(string_of(record.body.as_ref().unwrap()), "grpc transport test");

    let seen = &collector.seen("traces")[0];
    assert_absent(&seen.message, canaries.secrets(), "the gRPC traces request");
    let decoded = ExportTraceServiceRequest::decode(seen.message.as_slice()).unwrap();
    assert_eq!(decoded.resource_spans[0].scope_spans[0].spans[0].name, "exporter.tick");
    // both signals travelled under the test resource, not the collector's own
    let resources = [
        decoded_logs(&collector.seen("logs")[0]).resource_logs[0]
            .resource
            .clone()
            .unwrap(),
        decoded.resource_spans[0].resource.clone().unwrap(),
    ];
    for resource in resources {
        let service = resource
            .attributes
            .iter()
            .find(|kv| kv.key == "service.name")
            .map(|kv| string_of(kv.value.as_ref().unwrap()))
            .unwrap();
        assert_eq!(service, SERVICE);
    }
}

#[test]
fn every_grpc_status_maps_to_the_outcome_the_table_requires() {
    let table = [
        (Code::Cancelled, GrpcClass::Retry(None)),
        (Code::DeadlineExceeded, GrpcClass::Retry(None)),
        (Code::Aborted, GrpcClass::Retry(None)),
        (Code::OutOfRange, GrpcClass::Retry(None)),
        (Code::DataLoss, GrpcClass::Retry(None)),
        (Code::Unavailable, GrpcClass::Retry(None)),
        (Code::Ok, GrpcClass::Retry(None)), // defensive: an Ok code is never an error
        (Code::Unauthenticated, GrpcClass::Unauthorized),
        (Code::PermissionDenied, GrpcClass::Unauthorized),
        (Code::InvalidArgument, GrpcClass::Refused),
        (Code::Unknown, GrpcClass::Refused),
        (Code::NotFound, GrpcClass::Refused),
        (Code::Unimplemented, GrpcClass::Refused),
        (Code::AlreadyExists, GrpcClass::Refused),
        (Code::FailedPrecondition, GrpcClass::Refused),
        (Code::Internal, GrpcClass::Refused),
    ];
    for (code, expected) in table {
        assert_eq!(classify_status(&Status::new(code, "boom")), expected, "{code:?}");
    }
}

/// A serialized `google.rpc.Status` whose details hold a `RetryInfo` of `seconds`/`nanos`.
fn retry_info(seconds: i64, nanos: i32) -> Vec<u8> {
    rich_error::Status {
        code: Code::ResourceExhausted as i32,
        message: "queue full".into(),
        details: vec![rich_error::Any {
            type_url: "type.googleapis.com/google.rpc.RetryInfo".into(),
            value: rich_error::RetryInfo {
                retry_delay: Some(rich_error::Duration { seconds, nanos }),
            }
            .encode_to_vec(),
        }],
    }
    .encode_to_vec()
}

#[test]
fn resource_exhausted_retries_only_with_a_retry_info_delay() {
    // no rich-error detail at all
    assert_eq!(
        classify_status(&Status::new(Code::ResourceExhausted, "queue full")),
        GrpcClass::Refused
    );
    // a detail, but not a RetryInfo
    let other = rich_error::Status {
        code: Code::ResourceExhausted as i32,
        message: "queue full".into(),
        details: vec![rich_error::Any {
            type_url: "type.googleapis.com/google.rpc.DebugInfo".into(),
            value: Vec::new(),
        }],
    }
    .encode_to_vec();
    assert_eq!(
        classify_status(&Status::with_details(Code::ResourceExhausted, "queue full", other.into())),
        GrpcClass::Refused
    );
    // a RetryInfo: retry after its delay
    assert_eq!(
        classify_status(&Status::with_details(
            Code::ResourceExhausted,
            "queue full",
            retry_info(7, 500_000_000).into()
        )),
        GrpcClass::Retry(Some(Duration::new(7, 500_000_000)))
    );
    // capped like the HTTP transport caps `Retry-After`, and a negative delay is no delay
    assert_eq!(
        classify_status(&Status::with_details(
            Code::ResourceExhausted,
            "queue full",
            retry_info(999_999, 0).into()
        )),
        GrpcClass::Retry(Some(MAX_RETRY_AFTER))
    );
    assert_eq!(
        classify_status(&Status::with_details(
            Code::ResourceExhausted,
            "queue full",
            retry_info(-1, 0).into()
        )),
        GrpcClass::Refused
    );
    // undecodable details are no details
    assert_eq!(
        classify_status(&Status::with_details(
            Code::ResourceExhausted,
            "queue full",
            vec![0xff, 0xff].into()
        )),
        GrpcClass::Refused
    );
}

#[tokio::test]
async fn a_status_that_quotes_a_header_is_scrubbed() {
    let canaries = Canaries::new();
    let token = bearer(&canaries);
    let collector = Collector::start(Arc::new({
        let token = token.clone();
        move |_| Reply::Failed(Status::unauthenticated(format!("Bearer {token} is not a valid key")))
    }))
    .await;
    let transport = Transport::new(
        &settings(&collector.url),
        std::path::Path::new("."),
        canary_headers(&canaries),
    )
    .unwrap();
    match transport.send(&logs_request("auth")).await {
        Outcome::Unauthorized { status, message } => {
            assert_eq!(status, Code::Unauthenticated as u16);
            assert!(!message.contains(&token), "{message}");
        }
        other => panic!("an unauthenticated status must hold the batch, not {other:?}"),
    }
}

#[tokio::test]
async fn a_dead_endpoint_is_a_scrubbed_retry() {
    let canaries = Canaries::new();
    // Port 9 (discard) on loopback: nothing listens, so the lazy channel's first call fails.
    let transport = Transport::new(
        &settings("http://127.0.0.1:9"),
        std::path::Path::new("."),
        canary_headers(&canaries),
    )
    .unwrap();
    match transport.send(&logs_request("dead")).await {
        Outcome::Retry { message, after, .. } => {
            assert_eq!(after, None);
            assert!(!message.contains(&bearer(&canaries)), "{message}");
            assert!(!message.contains(&api_key(&canaries)), "{message}");
        }
        other => panic!("a dead endpoint must retry, not {other:?}"),
    }
}

#[test]
fn operator_headers_become_metadata_and_owned_ones_are_dropped() {
    let map = metadata_map(&[
        ("content-type".into(), "application/grpc".into()),
        ("Content-Length".into(), "9".into()),
        ("content-encoding".into(), "gzip".into()),
        ("Host".into(), "otlp.example.com".into()),
        ("te".into(), "trailers".into()),
        ("authorization".into(), "value-one".into()),
        ("x-api-key".into(), "value-two".into()),
    ])
    .unwrap();
    assert_eq!(map.keys_len(), 2, "the channel's own headers are dropped");
    assert_eq!(map.get("authorization").unwrap().to_str().unwrap(), "value-one");
    assert_eq!(map.get("x-api-key").unwrap().to_str().unwrap(), "value-two");
}

#[test]
fn metadata_a_name_or_value_cannot_carry_is_refused_without_quoting_it() {
    for headers in [
        vec![("bad name".to_string(), "value".to_string())],
        vec![("k".to_string(), "value\u{7f}".to_string())],
        vec![("k".to_string(), "value\u{e9}".to_string())],
    ] {
        let err = metadata_map(&headers).unwrap_err();
        assert!(err.starts_with("header "), "{err}");
        assert!(!err.contains('\u{7f}') && !err.contains('\u{e9}'), "{err}");
    }
}

#[tokio::test]
async fn grpc_endpoints_must_name_a_scheme_the_transport_speaks() {
    for endpoint in ["grpc://otlp.example.com", "otlp.example.com:4317", "unix:/tmp/otlp.sock"] {
        let mut s = settings("");
        s.endpoint = endpoint.into();
        let err = GrpcTransport::new(&s, Vec::new()).unwrap_err();
        assert!(err.contains("http:// or https://"), "{err}: {endpoint}");
    }
    // an empty base is an error unless the signal carries its own endpoint
    assert!(GrpcTransport::new(&settings(""), Vec::new()).is_err());
    let mut s = settings("");
    s.endpoint = "https://otlp.example.com/".into();
    s.logs_endpoint = Some("http://127.0.0.1:14317".into());
    assert!(GrpcTransport::new(&s, Vec::new()).is_ok());
}
