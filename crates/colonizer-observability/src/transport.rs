//! OTLP/HTTP (#849): POSTs an encoded request to the signal's URL with the operator's headers, and
//! sorts the answer into sent, retry later, unauthorized, or refused. Only a refusal that is about
//! the records (400, 413, 422) may lead to a record being dropped; every other failure keeps the
//! batch for a later retry. Header values never appear in an error: an
//! error is built from a status code and a redacted, capped slice of the response body, and every
//! header value is cut out of it again before it leaves this module.

use crate::contract::Settings;
use crate::encode::{Encoding, Request, gzip};
use crate::proto::collector::logs::v1::ExportLogsServiceResponse;
use crate::proto::collector::metrics::v1::ExportMetricsServiceResponse;
use crate::proto::collector::trace::v1::ExportTraceServiceResponse;
use prost::Message;
use std::time::{Duration, SystemTime};

/// The longest a `Retry-After` is honoured for; a longer one waits this long and asks again.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// What became of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Accepted; `rejected` records were refused by the backend's partial success and are not
    /// retried (an ack is an ack). `message` is the partial success's `error_message`, if any (a
    /// warning when `rejected` is 0). `bytes` is the request body as it went on the wire.
    Sent {
        rejected: u64,
        message: Option<String>,
        bytes: u64,
    },
    /// Worth retrying after a backoff: the network, a timeout, 408, 429, 5xx, or a status that says
    /// nothing about the records (404, 405, 415, …). `after` is the server's `Retry-After` (429 and
    /// 503 only), capped at [`MAX_RETRY_AFTER`].
    Retry { message: String, after: Option<Duration> },
    /// The credential was refused (401, 403, 407): the records are fine, the key is not. Nothing is
    /// dropped; the batch waits for a working key.
    Unauthorized { status: u16, message: String },
    /// Refused for this request's records (400, 413, 422): retrying it unchanged cannot help, so
    /// the exporter bisects it down to the record at fault.
    Refused { status: u16, message: String },
}

/// How a non-2xx status is handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Retry,
    Unauthorized,
    Refused,
}

/// Sorts a non-2xx status. Only statuses that name or imply bad records are `Refused`; an unknown
/// status keeps the batch (the ledgers are the spool, so holding costs nothing but time).
pub fn classify(status: u16) -> Class {
    match status {
        401 | 403 | 407 => Class::Unauthorized,
        400 | 413 | 422 => Class::Refused,
        _ => Class::Retry,
    }
}

/// A `Retry-After` value, either delay-seconds or an HTTP-date, as a delay from `now`, capped at
/// [`MAX_RETRY_AFTER`]. A date in the past is no delay; anything unparseable is `None`.
pub fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    let delay = if let Ok(secs) = value.parse::<u64>() {
        Duration::from_secs(secs)
    } else {
        let at = http_date(value)?;
        at.duration_since(now).unwrap_or(Duration::ZERO)
    };
    Some(delay.min(MAX_RETRY_AFTER))
}

/// An HTTP-date (RFC 9110 §5.6.7): the IMF-fixdate form, and the obsolete RFC 850 and asctime forms.
fn http_date(value: &str) -> Option<SystemTime> {
    use chrono::{DateTime, NaiveDateTime};
    let parsed = DateTime::parse_from_rfc2822(value)
        .map(|d| d.naive_utc())
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%A, %d-%b-%y %H:%M:%S GMT"))
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%a %b %e %H:%M:%S %Y"))
        .ok()?;
    let secs = parsed.and_utc().timestamp();
    let secs = u64::try_from(secs).ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
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
        let wire_bytes = body.len() as u64;
        let response = match builder.body(body).send().await {
            Ok(r) => r,
            // reqwest's error names the URL, which carries no credential (the mothership refuses
            // userinfo and query strings), never a header.
            Err(e) => {
                return Outcome::Retry {
                    message: self.scrub(&format!("{url}: {}", without_url(&e))),
                    after: None,
                };
            }
        };
        let status = response.status().as_u16();
        let after = match status {
            429 | 503 => response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| retry_after(v, SystemTime::now())),
            _ => None,
        };
        let bytes = response.bytes().await.unwrap_or_default();
        if (200..300).contains(&status) {
            let (rejected, message) = partial_success(request, self.encoding, &bytes);
            return Outcome::Sent {
                rejected,
                message: message.map(|m| self.scrub(&cap(&m))),
                bytes: wire_bytes,
            };
        }
        let snippet = String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned();
        let message = self.scrub(&format!("{url} answered {status}: {}", snippet.trim()));
        match classify(status) {
            Class::Retry => Outcome::Retry { message, after },
            Class::Unauthorized => Outcome::Unauthorized { status, message },
            Class::Refused => Outcome::Refused { status, message },
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

/// `text` cut to 300 bytes on a character boundary.
fn cap(text: &str) -> String {
    let mut end = text.len().min(300);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim().to_string()
}

/// A 2xx answer's `partial_success`: how many records it says were refused, and its message.
pub(crate) fn partial_success(request: &Request, encoding: Encoding, body: &[u8]) -> (u64, Option<String>) {
    if body.is_empty() {
        return (0, None);
    }
    let (n, message) = match encoding {
        Encoding::Protobuf => match request {
            Request::Logs(_) => ExportLogsServiceResponse::decode(body)
                .ok()
                .and_then(|r| r.partial_success)
                .map(|p| (p.rejected_log_records, p.error_message)),
            Request::Metrics(_) => ExportMetricsServiceResponse::decode(body)
                .ok()
                .and_then(|r| r.partial_success)
                .map(|p| (p.rejected_data_points, p.error_message)),
            Request::Traces(_) => ExportTraceServiceResponse::decode(body)
                .ok()
                .and_then(|r| r.partial_success)
                .map(|p| (p.rejected_spans, p.error_message)),
        }
        .unwrap_or((0, String::new())),
        Encoding::Json => serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|v| {
                let p = v.get("partialSuccess")?;
                let n = ["rejectedLogRecords", "rejectedDataPoints", "rejectedSpans"]
                    .iter()
                    .find_map(|k| p.get(*k))
                    .and_then(|n| n.as_i64().or_else(|| n.as_str().and_then(|s| s.parse().ok())))
                    .unwrap_or(0);
                let m = p.get("errorMessage").and_then(|m| m.as_str()).unwrap_or("").to_string();
                Some((n, m))
            })
            .unwrap_or((0, String::new())),
    };
    let message = Some(message).filter(|m| !m.trim().is_empty());
    (n.max(0) as u64, message)
}

#[cfg(test)]
mod tests;
