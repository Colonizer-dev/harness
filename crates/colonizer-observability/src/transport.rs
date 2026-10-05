//! OTLP/HTTP (#849): POSTs an encoded request to the signal's URL with the operator's headers, and
//! sorts the answer into sent, retry later, or refused. Header values never appear in an error: an
//! error is built from a status code and a redacted, capped slice of the response body, and every
//! header value is cut out of it again before it leaves this module.

use crate::contract::Settings;
use crate::encode::{Encoding, Request, gzip};
use crate::proto::collector::logs::v1::ExportLogsServiceResponse;
use crate::proto::collector::metrics::v1::ExportMetricsServiceResponse;
use crate::proto::collector::trace::v1::ExportTraceServiceResponse;
use prost::Message;
use std::time::Duration;

/// What became of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Accepted; `rejected` records were refused by the backend's partial success and are not
    /// retried (an ack is an ack).
    Sent { rejected: u64 },
    /// Worth retrying after a backoff: the network, a timeout, 408, 429, 502, 503 or 504.
    Retry(String),
    /// Refused for this request's content (400, 413, …): retrying it unchanged cannot help.
    Refused { status: u16, message: String },
}

/// One OTLP/HTTP destination.
pub struct Transport {
    client: reqwest::Client,
    settings: Settings,
    headers: Vec<(String, String)>,
    encoding: Encoding,
    gzip: bool,
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<_> = self.headers.iter().map(|(n, _)| format!("{n}=<redacted>")).collect();
        f.debug_struct("Transport")
            .field("endpoint", &self.settings.endpoint)
            .field("encoding", &self.encoding)
            .field("gzip", &self.gzip)
            .field("headers", &names)
            .finish()
    }
}

impl Transport {
    pub fn new(settings: &Settings, headers: Vec<(String, String)>) -> Result<Transport, String> {
        let encoding = match settings.protocol.as_str() {
            "http/protobuf" | "" => Encoding::Protobuf,
            "http/json" => Encoding::Json,
            other => {
                return Err(format!(
                    "protocol {other} is not supported by this build; use http/protobuf or http/json"
                ));
            }
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(settings.timeout_secs.clamp(1, 120)))
            .user_agent(concat!("colonizer-observability/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {e}"))?;
        Ok(Transport {
            client,
            settings: settings.clone(),
            headers,
            encoding,
            gzip: settings.compression != "none",
        })
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Sends one request and classifies the answer.
    pub async fn send(&self, request: &Request) -> Outcome {
        let url = self.settings.url(request.path());
        let mut body = request.encode(self.encoding);
        let mut builder = self.client.post(&url).header(
            "content-type",
            match self.encoding {
                Encoding::Protobuf => "application/x-protobuf",
                Encoding::Json => "application/json",
            },
        );
        if self.gzip {
            body = gzip(&body);
            builder = builder.header("content-encoding", "gzip");
        }
        for (name, value) in &self.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let response = match builder.body(body).send().await {
            Ok(r) => r,
            // reqwest's error names the URL, which carries no credential (the mothership refuses
            // userinfo and query strings), never a header.
            Err(e) => return Outcome::Retry(self.scrub(&format!("{url}: {}", without_url(&e)))),
        };
        let status = response.status().as_u16();
        let bytes = response.bytes().await.unwrap_or_default();
        if (200..300).contains(&status) {
            return Outcome::Sent {
                rejected: rejected(request, self.encoding, &bytes),
            };
        }
        let snippet = String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned();
        let message = self.scrub(&format!("{url} answered {status}: {}", snippet.trim()));
        match status {
            408 | 429 | 502 | 503 | 504 => Outcome::Retry(message),
            _ => Outcome::Refused { status, message },
        }
    }

    /// `text` redacted, with every header value removed even if the redactor would not know it.
    fn scrub(&self, text: &str) -> String {
        let mut out = colonizer_redact::redact_text(text).into_owned();
        for (_, value) in &self.headers {
            if value.len() >= 4 {
                out = out.replace(value.as_str(), "<redacted>");
            }
        }
        out
    }
}

/// A reqwest error's text without the URL it carries (the caller names the URL itself).
fn without_url(e: &reqwest::Error) -> String {
    let mut chain = Vec::new();
    let mut source: Option<&dyn std::error::Error> = Some(e);
    while let Some(err) = source {
        chain.push(err.to_string());
        source = err.source();
    }
    if e.is_timeout() {
        return "timed out".into();
    }
    chain.last().cloned().unwrap_or_else(|| "request failed".into())
}

/// The records a 2xx answer's `partial_success` says were refused.
fn rejected(request: &Request, encoding: Encoding, body: &[u8]) -> u64 {
    if body.is_empty() {
        return 0;
    }
    let n = match encoding {
        Encoding::Protobuf => match request {
            Request::Logs(_) => ExportLogsServiceResponse::decode(body)
                .ok()
                .and_then(|r| r.partial_success)
                .map(|p| p.rejected_log_records),
            Request::Metrics(_) => ExportMetricsServiceResponse::decode(body)
                .ok()
                .and_then(|r| r.partial_success)
                .map(|p| p.rejected_data_points),
            Request::Traces(_) => ExportTraceServiceResponse::decode(body)
                .ok()
                .and_then(|r| r.partial_success)
                .map(|p| p.rejected_spans),
        },
        Encoding::Json => serde_json::from_slice::<serde_json::Value>(body).ok().and_then(|v| {
            let p = v.get("partialSuccess")?;
            ["rejectedLogRecords", "rejectedDataPoints", "rejectedSpans"]
                .iter()
                .find_map(|k| p.get(*k))
                .and_then(|n| n.as_i64().or_else(|| n.as_str().and_then(|s| s.parse().ok())))
        }),
    };
    n.unwrap_or(0).max(0) as u64
}
